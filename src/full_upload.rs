use core::str;
use std::io::Write;

use axum::{extract::Multipart, http::StatusCode, response::IntoResponse};

use crate::{backend::{PreflightRequest, RegisterRequest}, file_meta::FileMetaService, Context};

pub async fn post(
	ctx:Context,
	meta_service:FileMetaService,
	mut multipart: Multipart,
)->axum::response::Response{
	let mut req=PreflightRequest::default();
	let mut file_data=None;
	let mut force=false;
	while let Some(field) = multipart.next_field().await.unwrap() {
		let name = field.name().unwrap().to_string();
		let data = field.bytes().await.unwrap();

		if &name=="name"{
			req.name=String::from_utf8(data.to_vec()).ok();
		}
		if &name=="ext"{
			req.ext=String::from_utf8(data.to_vec()).ok();
		}
		if &name=="folder_id"{
			req.folder_id=String::from_utf8(data.to_vec()).ok();
		}
		if &name=="i"{
			req.i=String::from_utf8(data.to_vec()).ok();
		}
		if &name=="isSensitive"{
			req.is_sensitive=match str::from_utf8(&data){
				Ok("true")=>true,
				Ok("false")=>false,
				_=>false,
			}
		}
		if &name=="force"{
			force=match str::from_utf8(&data){
				Ok("true")=>true,
				Ok("false")=>false,
				_=>false,
			}
		}
		if &name=="size"{
			req.size=match str::from_utf8(&data){
				Ok(s)=>u64::from_str_radix(s,10).unwrap_or_default(),
				_=>0,
			}
		}
		if &name=="file"{
			file_data=Some(data);
		}
	}
	if file_data.is_none(){
		let mut header=axum::http::header::HeaderMap::new();
		header.insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,"*".parse().unwrap());
		return (axum::http::StatusCode::BAD_REQUEST,header).into_response();
	}
	let file_data=file_data.unwrap();

	let mut content_type="";
	if let Some(kind)=infer::get(&file_data){
		content_type=kind.mime_type();
		req.ext=Some(format!(".{}",kind.extension()));
		//println!("known content_type:{}",content_type);
	}
	if req.ext.as_ref().map(|s|s.as_str()) == Some("") {
		req.ext=match content_type{
			"image/jpeg"=>Some(".jpg"),
			"image/png"=>Some(".png"),
			"image/webp"=>Some(".webp"),
			"image/avif"=>Some(".avif"),
			"image/apng"=>Some(".apng"),
			"image/vnd.mozilla.apng"=>Some(".apng"),
			_=>None,
		}.map(|s|s.to_owned());
	}
	if content_type == "image/apng"{
		content_type="image/png";
	}
	if !crate::browsersafe::FILE_TYPE_BROWSERSAFE.contains(&content_type){
		content_type = "application/octet-stream";
		req.ext = None;
	}
	//let offset_time=chrono::Utc::now();
	let res=ctx.backend.preflight(&mut req).await;
	//println!("preflight{}ms",(chrono::Utc::now()-offset_time).num_milliseconds());
	if let Err(e)=res{
		let mut header=axum::http::header::HeaderMap::new();
		ctx.config.set_cors_header(&mut header);
		return (axum::http::StatusCode::BAD_REQUEST,header,e).into_response();
	}
	let res=res.unwrap();
	//println!("PREFLIGHT {:?}",res);

	let s3_key=format!("{}/{}{}",ctx.config.prefix,uuid::Uuid::new_v4().to_string(),req.ext.as_ref().map(|s|s.as_str()).unwrap_or(""));
	let mut md5sum=md5::Context::new();
	let (md5sum,content_md5) =match md5sum.write_all(&file_data){
		Ok(_)=>{
			let md5sum=md5sum.compute().0;
			let content_md5=s3::command::ContentMd5::from(md5sum.as_ref());
			let md5sum=md5sum.iter().map(|n| format!("{:02x}", n)).collect::<String>();
			(md5sum,content_md5)
		},
		Err(_)=>(String::new(),s3::command::ContentMd5::None)
	};

	//let offset_time=chrono::Utc::now();
	let thumbnail_size=2048;
	let cache_control="max-age=31536000, immutable";
	let detected_name=percent_encoding::percent_encode(res.detected_name.as_bytes(), percent_encoding::NON_ALPHANUMERIC);
	let content_disposition=format!("inline; filename=\"{}\"",detected_name);
	
	let (raw_upload,(mut thumbnail_upload,mut info))=futures_util::join!(
		ctx.bucket.put_object_with_metadata(&s3_key,&file_data,&content_type,content_md5.clone(),cache_control,&content_disposition),
		async{
			let info=match image::load_from_memory(&file_data){
				Ok(img)=>meta_service.metadata(
					img,
					res.sensitive_threshold,
					res.skip_sensitive_detection,
					thumbnail_size,
					ctx.config.thumbnail_quality,
					ctx.config.thumbnail_filter.into(),
				).await,
				_=>Default::default(),
			};
			let thumbnail_bin=info.thumbnail.as_ref();
			(match thumbnail_bin{
				Some(thumbnail_bin)=>{
					let thumbnail_key=format!("{}/thumbnail-{}{}",ctx.config.prefix,uuid::Uuid::new_v4().to_string(),".webp");
					match ctx.bucket.put_object_with_metadata(&thumbnail_key,&thumbnail_bin,"image/webp",s3::command::ContentMd5::Auto,cache_control,&content_disposition).await{
						Ok(_)=>Ok(Some(thumbnail_key)),
						Err(e)=>Err(e),
					}
				},
				None=>Ok(None)
			},info)
		}
	);
	if content_type.starts_with("video/"){
		info=meta_service.ffmpeg_metadata(&ctx.config,&s3_key,thumbnail_size,res.sensitive_threshold,res.skip_sensitive_detection).await.unwrap_or_default();
		let thumbnail_bin=info.thumbnail.as_ref();
		thumbnail_upload=match thumbnail_bin{
			Some(thumbnail_bin)=>{
				let thumbnail_key=format!("{}/thumbnail-{}{}",ctx.config.prefix,uuid::Uuid::new_v4().to_string(),".webp");
				match ctx.bucket.put_object_with_metadata(&thumbnail_key,&thumbnail_bin,"image/webp",s3::command::ContentMd5::Auto,cache_control,&content_disposition).await{
					Ok(_)=>Ok(Some(thumbnail_key)),
					Err(e)=>Err(e),
				}
			},
			None=>Ok(None)
		};
	}
	//println!("name:{}",res.detected_name);
	//println!("md5sum:{}",md5sum);
	//println!("sensitive:{}",info.maybe_sensitive.unwrap_or_default());
	//println!("blurhash:{}",info.blurhash.clone().unwrap_or_default());
	//println!("metadata{}ms",(chrono::Utc::now()-offset_time).num_milliseconds());
	//println!("s3_key:{}",&s3_key);
	match raw_upload{
		Ok(_resp) => {},
		Err(e) =>{
			eprintln!("{}:{} {:?}",file!(),line!(),e);
			return StatusCode::INTERNAL_SERVER_ERROR.into_response();
		},
	}
	let thumbnail_key=match thumbnail_upload{
		Ok(key) => {
			key
		},
		Err(e) =>{
			eprintln!("{}:{} {:?}",file!(),line!(),e);
			return StatusCode::INTERNAL_SERVER_ERROR.into_response();
		},
	};
	let mut req=RegisterRequest{
		upload_service_key: None,
		base_url: ctx.config.public_base_url.clone(),
		access_key: s3_key,
		thumbnail_key,
		md5: md5sum,
		blurhash: info.blurhash,
		size: file_data.len() as u64,
		width: info.width,
		height: info.height,
		source_url: None,
		remote_uri: None,
		is_link: false,
		folder_id: req.folder_id,
		name: res.detected_name,
		comment: req.comment,
		is_sensitive: req.is_sensitive,
		maybe_sensitive: info.maybe_sensitive.unwrap_or_default(),
		content_type: content_type.to_owned(),
		force,
		i: req.i,
		user_id: req.user_id,
	};
	let res=ctx.backend.register(&mut req).await;
	if let Err(e)=res{
		let mut header=axum::http::header::HeaderMap::new();
		ctx.config.set_cors_header(&mut header);
		return (axum::http::StatusCode::BAD_REQUEST,header,e).into_response();
	}
	let (status,res)=res.unwrap();
	let mut header=axum::http::header::HeaderMap::new();
	ctx.config.set_cors_header(&mut header);
	header.insert(axum::http::header::CONTENT_TYPE,"application/json".parse().unwrap());
	let status=axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
	(status,header,res).into_response()
}
