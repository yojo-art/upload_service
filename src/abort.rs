use axum::{http::StatusCode, response::IntoResponse};

use crate::Context;

pub async fn post(
		mut ctx:Context,
		request: axum::extract::Request,
	)->axum::response::Response{
	let authorization=request.headers().get("Authorization");
	let (session,_hashed_sid)=match ctx.upload_session(authorization,true).await{
		Ok(v)=>v,
		Err(e)=>return e,
	};
	if let Some(upload_id)=session.upload_id.as_ref(){
		let _=ctx.bucket.abort_upload(&session.s3_key,upload_id).await;
	}
	let mut header=axum::http::header::HeaderMap::new();
	ctx.config.set_cors_header(&mut header);
	(StatusCode::NO_CONTENT,header).into_response()
}
