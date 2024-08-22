use axum::{http::StatusCode, response::IntoResponse};
use futures::TryStreamExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::io::StreamReader;

use crate::{backend::PreflightRequest, Context, UploadSession};

#[derive(Debug, Deserialize)]
pub struct RequestParams{
	i: String,//トークン必須
	content_length:Option<u64>,
	#[serde(rename = "folderId")]
	folder_id:Option<String>,
	name:Option<String>,
	#[serde(rename = "isSensitive")]
	is_sensitive:bool,
	comment:Option<String>,
	force:bool,
}
#[derive(Debug, Serialize)]
pub struct ResponseBody{
	allow_upload:bool,
	min_split_size:u32,
	max_split_size:u64,
	session_id:String,
}
pub async fn post(
	mut ctx:Context,
	request: axum::extract::Request,
)->axum::response::Response{
	let min_size=5*1024*1024;//最小5MB
	let stream=request.into_body().into_data_stream();
	let body_with_io_error = stream.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err));
	let mut body_reader = StreamReader::new(body_with_io_error);
	let mut buf=vec![];
	if let Err(e)=body_reader.read_to_end(&mut buf).await{
		eprintln!("{}:{} {:?}",file!(),line!(),e);
		let mut header=axum::http::header::HeaderMap::new();
		ctx.config.set_cors_header(&mut header);
		return (StatusCode::INTERNAL_SERVER_ERROR,header).into_response();
	}
	let q=match serde_json::from_slice::<RequestParams>(&buf){
		Ok(v)=>v,
		Err(e)=>{
			eprintln!("{}:{} {:?}",file!(),line!(),e);
			let mut header=axum::http::header::HeaderMap::new();
			ctx.config.set_cors_header(&mut header);
			return (StatusCode::BAD_REQUEST,header).into_response();
		}
	};
	let mut req=PreflightRequest::default();
	req.i=Some(q.i);
	req.comment=q.comment;
	req.folder_id=q.folder_id;
	req.is_sensitive=q.is_sensitive;
	req.name=q.name;
	req.size=q.content_length.unwrap_or_default();
	//let offset_time=chrono::Utc::now();
	let res=ctx.backend.preflight(&mut req).await;
	//println!("preflight{}ms",(chrono::Utc::now()-offset_time).num_milliseconds());
	if let Err(e)=res{
		let mut header=axum::http::header::HeaderMap::new();
		ctx.config.set_cors_header(&mut header);
		return (axum::http::StatusCode::BAD_REQUEST,header,e).into_response();
	}
	let backend_res=res.unwrap();
	//println!("PREFLIGHT {:?}",res);
	println!("content_length:{:?}",q.content_length);
	let mut res=ResponseBody{
		allow_upload:true,
		min_split_size:min_size,
		max_split_size:ctx.config.part_max_size,
		session_id:uuid::Uuid::new_v4().to_string(),
	};
	let s3_key=format!("{}/{}",ctx.config.prefix,uuid::Uuid::new_v4().to_string());
	//進行中の分割アップロードの一覧が取れる。これを使って適当に掃除する
	//bucket.list_multiparts_uploads(Some("/"), Some("/"));
	let md5_ctx_64=crate::md5_ontext_into_raw(md5::Context::new());
	let session=UploadSession{
		s3_key,
		part_number:None,
		content_length:0,
		upload_id:None,
		content_type:"application/octet-stream".to_owned(),
		part_etag:vec![],
		md5_ctx_64,
		ext: None,
		comment:req.comment,
		folder_id:req.folder_id,
		is_sensitive:req.is_sensitive,
		name:backend_res.detected_name,
		force:q.force,
		sensitive_threshold:backend_res.sensitive_threshold,
		skip_sensitive_detection:backend_res.skip_sensitive_detection,
	};
	let session=serde_json::to_string(&session).unwrap();
	let time_out=30;//暫定セッション更新期限30秒
	let sid={
		use sha2::{Sha256, Digest};
		let mut hasher = Sha256::new();
		hasher.update(res.session_id.as_bytes());
		let hash=hasher.finalize();
		use base64::Engine;
		base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
	};
	let mut header=axum::http::header::HeaderMap::new();
	ctx.config.set_cors_header(&mut header);
	if ctx.redis.set_ex::<&String,String,()>(&sid,session,time_out).await.is_ok(){
		(StatusCode::OK,header,serde_json::to_string(&res).unwrap()).into_response()
	}else{
		res.allow_upload=false;
		(StatusCode::INTERNAL_SERVER_ERROR,header,serde_json::to_string(&res).unwrap()).into_response()
	}
}
