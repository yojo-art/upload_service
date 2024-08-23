use std::io::Write;

use axum::{http::StatusCode, response::IntoResponse};
use futures::TryStreamExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::io::StreamReader;

use crate::Context;

#[derive(Debug,Serialize, Deserialize)]
pub struct RequestParams{
	partnumber: u32,//トークン必須
}
pub async fn post(
	mut ctx:Context,
	axum::extract::Query(parms):axum::extract::Query<RequestParams>,
	request: axum::extract::Request,
)->axum::response::Response{
	let authorization=request.headers().get("Authorization").cloned();
	let _=match ctx.upload_session(authorization.as_ref(),false).await{
		Ok(v)=>v,
		Err(e)=>return e,
	};
	let body=request.into_body();
	let body=body.into_data_stream();
	let body_with_io_error = body.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err));
	let body_reader = StreamReader::new(body_with_io_error);
	futures::pin_mut!(body_reader);
	//println!("{:?}",session);
	let buf={
		let mut all_body=vec![];
		let mut buf=vec![];
		loop{
			match body_reader.read(&mut buf).await{
				Ok(len)=>{
					if len==0{
						break;
					}
					if all_body.len()+len>ctx.config.part_max_size as usize{
						let mut header=axum::http::header::HeaderMap::new();
						ctx.config.set_cors_header(&mut header);
						return (StatusCode::PAYLOAD_TOO_LARGE,header).into_response();
					}
					all_body.extend_from_slice(&buf[0..len]);
				},
				Err(e)=>{
					eprintln!("{}:{} {:?}",file!(),line!(),e);
					let mut header=axum::http::header::HeaderMap::new();
					ctx.config.set_cors_header(&mut header);
					return (StatusCode::INTERNAL_SERVER_ERROR,header).into_response();
				}
			}
		}
		all_body
	};
	let (mut session,hashed_sid)=match ctx.upload_session(authorization.as_ref(),true).await{
		Ok(v)=>v,
		Err(e)=>return e,
	};
	//bucket.abort_upload(key, upload_id)
	if let Some(v)=session.part_number.as_mut(){
		if *v+1 == parms.partnumber{
			*v+=1;
		}else{
			let mut header=axum::http::header::HeaderMap::new();
			ctx.config.set_cors_header(&mut header);
			return (StatusCode::BAD_REQUEST,header).into_response();
		}
	}else{
		if parms.partnumber==0{
			session.part_number=Some(0);
		}else{
			let mut header=axum::http::header::HeaderMap::new();
			ctx.config.set_cors_header(&mut header);
			return (StatusCode::BAD_REQUEST,header).into_response();
		}
	}
	if session.part_number==Some(0){
		//最初のオブジェクト
		let mut ext=None;
		let mut content_type="";
		if let Some(kind)=infer::get(&buf){
			content_type=kind.mime_type();
			ext=Some(format!(".{}",kind.extension()));
			//println!("known content_type:{}",content_type);
		}
		if ext.as_ref().map(|s|s.as_str()) == Some("") {
			ext=match content_type{
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
			ext = None;
		}
		session.content_type=content_type.to_owned();
		session.ext=ext;
		session.upload_id=match ctx.bucket.initiate_multipart_upload(&session.s3_key,content_type).await{
			Ok(imur)=>{
				Some(imur.upload_id)
			},
			Err(e)=>{
				eprintln!("{}:{} {:?}",file!(),line!(),e);
				let mut header=axum::http::header::HeaderMap::new();
				ctx.config.set_cors_header(&mut header);
				return (StatusCode::INTERNAL_SERVER_ERROR,header).into_response();
			}
		};
	}
	//let start_time=chrono::Utc::now();
	let mut md5sum=crate::md5_ontext_from_raw(&session.md5_ctx_64);
	if let Err(e)=md5sum.write_all(&buf){
		eprintln!("{}:{} {:?}",file!(),line!(),e);
		let mut header=axum::http::header::HeaderMap::new();
		ctx.config.set_cors_header(&mut header);
		return (StatusCode::INTERNAL_SERVER_ERROR,header).into_response();
	}
	session.md5_ctx_64=crate::md5_ontext_into_raw(md5sum);
	//println!("md5 {}ms",(chrono::Utc::now()-start_time).num_milliseconds());
	session.content_length+=buf.len() as u64;
	let temp_id=format!("s3_wait_etag:{}",uuid::Uuid::new_v4().to_string());
	session.part_etag.push(temp_id.clone());
	match ctx.redis.set_ex::<&String,String,()>(&hashed_sid,serde_json::to_string(&session).unwrap(),ctx.config.redis.session_ttl).await{
		Ok(_)=>{},
		_=>return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
	}
	let mut redis=ctx.redis.clone();
	tokio::runtime::Handle::current().spawn(async move{
		match ctx.bucket.put_multipart_chunk(buf,&session.s3_key,parms.partnumber+1,&session.upload_id.unwrap(),&session.content_type).await{
			Ok(part)=>{
				let _=redis.set_ex::<String,String,()>(temp_id,part.etag,24*60*60).await;//24時間後に失敗する
			},
			Err(e)=>{
				eprintln!("{}:{} {:?}",file!(),line!(),e);
				//空文字列は失敗
				let _=redis.set_ex::<&str,&str,()>(temp_id.as_str(),"",24*60*60).await;//24時間後に失敗する
			}
		}
	});
	let mut header=axum::http::header::HeaderMap::new();
	ctx.config.set_cors_header(&mut header);
	(StatusCode::NO_CONTENT,header).into_response()
}
