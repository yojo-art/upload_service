use std::{io::{Read, Write}, net::SocketAddr, sync::Arc};

use axum::{http::StatusCode, response::{IntoResponse, Response}, Router};
use backend::BackendService;
use redis::aio::MultiplexedConnection;
use s3::Bucket;
use serde::{Deserialize, Serialize};
mod browsersafe;
mod abort;
mod backend;
mod preflight;
mod partial_upload;
mod finish_upload;
mod full_upload;
mod file_meta;

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ConfigFile{
	bind_addr: String,
	public_base_url:String,
	prefix:String,
	thumbnail_filter:FilterType,
	thumbnail_quality:f32,
	allow_origin:String,
	ffmpeg:Option<String>,
	ffmpeg_base_url:Option<String>,
	s3: S3Config,
	redis:RedisConfig,
	max_size:u64,
	backend:Backend,
}
impl ConfigFile{
	pub fn set_cors_header(&self,header:&mut axum::http::header::HeaderMap){
		header.insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,self.allow_origin.parse().unwrap());
		header.insert(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,"Origin, Authorization".parse().unwrap());
	}
}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Backend{
	endpoint: String,
	key: String,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct S3Config{
	endpoint: String,
	bucket: String,
	region: String,
	access_key: String,
	secret_key: String,
	timeout:u64,
	path_style:bool,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct RedisConfig{
	endpoint: String,
}
#[derive(Clone,Debug)]
pub struct Context{
	bucket:Box<Bucket>,
	config:Arc<ConfigFile>,
	redis:MultiplexedConnection,
	backend:BackendService,
}
#[derive(Clone, Copy,Debug,Serialize,Deserialize)]
enum FilterType{
	Nearest,
	Triangle,
	CatmullRom,
	Gaussian,
	Lanczos3,
}
impl Into<image::imageops::FilterType> for FilterType{
	fn into(self) -> image::imageops::FilterType {
		match self {
			FilterType::Nearest => image::imageops::Nearest,
			FilterType::Triangle => image::imageops::Triangle,
			FilterType::CatmullRom => image::imageops::CatmullRom,
			FilterType::Gaussian => image::imageops::Gaussian,
			FilterType::Lanczos3 => image::imageops::Lanczos3,
		}
	}
}
impl Into<fast_image_resize::FilterType> for FilterType{
	fn into(self) -> fast_image_resize::FilterType {
		match self {
			FilterType::Nearest => fast_image_resize::FilterType::Box,
			FilterType::Triangle => fast_image_resize::FilterType::Bilinear,
			FilterType::CatmullRom => fast_image_resize::FilterType::CatmullRom,
			FilterType::Gaussian => fast_image_resize::FilterType::Mitchell,
			FilterType::Lanczos3 => fast_image_resize::FilterType::Lanczos3,
		}
	}
}
async fn shutdown_signal() {
	use tokio::signal;
	use futures::{future::FutureExt,pin_mut};
	let ctrl_c = async {
		signal::ctrl_c()
			.await
			.expect("failed to install Ctrl+C handler");
	}.fuse();

	#[cfg(unix)]
	let terminate = async {
		signal::unix::signal(signal::unix::SignalKind::terminate())
			.expect("failed to install signal handler")
			.recv()
			.await;
	}.fuse();
	#[cfg(not(unix))]
	let terminate = std::future::pending::<()>().fuse();
	pin_mut!(ctrl_c, terminate);
	futures::select!{
		_ = ctrl_c => {},
		_ = terminate => {},
	}
}
fn main() {
	let config_path=match std::env::var("SUMMALY_CONFIG_PATH"){
		Ok(path)=>{
			if path.is_empty(){
				"config.json".to_owned()
			}else{
				path
			}
		},
		Err(_)=>"config.json".to_owned()
	};
	if !std::path::Path::new(&config_path).exists(){
		let default_config=ConfigFile{
			bind_addr: "0.0.0.0:12200".to_owned(),
			public_base_url:"https://files.example.com/".to_owned(),
			prefix:"prefix".to_owned(),
			thumbnail_filter:FilterType::Lanczos3,
			thumbnail_quality:50f32,
			max_size:20*1024*1024,
			allow_origin: "http://localhost:3000".to_owned(),
			ffmpeg:Some("ffmpeg".to_owned()),
			ffmpeg_base_url:Some("https://files.example.com/".to_owned()),
			s3:S3Config{
				endpoint: "localhost:9000".to_owned(),
				region: "us-east-1".to_owned(),
				access_key: "example-user".to_owned(),
				secret_key: "example-password".to_owned(),
				bucket: "files".to_owned(),
				timeout: 5000,
				path_style: true,
			},
			redis:RedisConfig{
				endpoint: "localhost:6379".to_owned(),
			},
			backend:Backend{
				endpoint: "http://localhost:3000/api".to_owned(),
				key: "default-upload-service-password".to_owned(),
			}
		};
		let default_config=serde_json::to_string_pretty(&default_config).unwrap();
		std::fs::File::create(&config_path).expect("create default config.json").write_all(default_config.as_bytes()).unwrap();
	}
	let file_service=file_meta::FileMetaService::new();
	let config:ConfigFile=serde_json::from_reader(std::fs::File::open(&config_path).unwrap()).unwrap();
	let config=Arc::new(config);
	let bucket = s3::Bucket::new(
		&config.s3.bucket,
		s3::Region::Custom {
			region: config.s3.region.to_owned(),
			endpoint: config.s3.endpoint.to_owned(),
		},
		s3::creds::Credentials::new(Some(&config.s3.access_key),Some(&config.s3.secret_key),None,None,None).unwrap(),
	).unwrap();
	let bucket=if config.s3.path_style{
		bucket.with_path_style()
	}else{
		bucket
	};
	let full_upload_limit=10*1024*1024;
	let redis=redis::Client::open(config.redis.endpoint.as_str()).unwrap();
	let rt=tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
	rt.block_on(async{
		let redis=redis.get_multiplexed_tokio_connection().await.unwrap();
		let backend=backend::BackendService::new(config.clone());
		let arg_tup=Context{
			bucket,
			config,
			redis,
			backend,
		};
		let http_addr:SocketAddr = arg_tup.config.bind_addr.parse().unwrap();
		let app = Router::new();
		let app=app.route("/",axum::routing::get(||async{
			let mut buf=String::new();
			std::fs::File::open("index.html").unwrap().read_to_string(&mut buf).unwrap();
			let mut header=axum::http::header::HeaderMap::new();
			header.insert(axum::http::header::CONTENT_TYPE,"text/html;charset=utf-8".parse().unwrap());
			(StatusCode::OK,header,buf).into_response()
		}));
		let arg_tup0=arg_tup.clone();
		let allow_origin0=arg_tup.config.clone();
		let app=app.route("/preflight",axum::routing::options(move||allow_cors(allow_origin0.clone())));
		let app=app.route("/preflight",axum::routing::post(move|parms|preflight::post(arg_tup0.clone(),parms)));
		let arg_tup0=arg_tup.clone();
		let allow_origin0=arg_tup.config.clone();
		let app=app.route("/partial-upload",axum::routing::options(move||allow_cors(allow_origin0.clone())));
		let app=app.route("/partial-upload",axum::routing::post(move|parms,body|partial_upload::post(arg_tup0.clone(),parms,body)));
		let arg_tup0=arg_tup.clone();
		let allow_origin0=arg_tup.config.clone();
		let app=app.route("/finish-upload",axum::routing::options(move||allow_cors(allow_origin0.clone())));
		let file_service0=file_service.clone();
		let app=app.route("/finish-upload",axum::routing::post(move|body|finish_upload::post(arg_tup0.clone(),file_service0,body)));
		let arg_tup0=arg_tup.clone();
		let allow_origin0=arg_tup.config.clone();
		let app=app.route("/create",axum::routing::options(move||allow_cors(allow_origin0.clone())));
		let app=app.route("/create",axum::routing::post(move|multipart|full_upload::post(arg_tup0.clone(),file_service,multipart))).layer(axum::extract::DefaultBodyLimit::max(full_upload_limit));
		let arg_tup0=arg_tup.clone();
		let allow_origin0=arg_tup.config.clone();
		let app=app.route("/abort",axum::routing::options(move||allow_cors(allow_origin0.clone())));
		let app=app.route("/abort",axum::routing::post(move|body|abort::post(arg_tup0.clone(),body)));
		let listener = tokio::net::TcpListener::bind(&http_addr).await.unwrap();
		axum::serve(listener,app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown_signal()).await.unwrap();
	});
}
async fn allow_cors(config:Arc<ConfigFile>)->Response{
	let mut header=axum::http::header::HeaderMap::new();
	config.set_cors_header(&mut header);
	(StatusCode::NO_CONTENT,header).into_response()
}
#[derive(Debug,Serialize, Deserialize)]
pub struct UploadSession{
	s3_key: String,
	upload_id:Option<String>,
	content_type:String,
	part_etag:Vec<String>,
	part_number:Option<u32>,
	content_length:u64,
	md5_ctx_64:String,
	ext:Option<String>,
	comment: Option<String>,
	folder_id: Option<String>,
	is_sensitive: bool,
	force:bool,
	name: String,
	sensitive_threshold: f32,
	skip_sensitive_detection: bool,
}
pub(crate) fn md5_ontext_into_raw(ctx:md5::Context)->String{
	let ptr=Box::leak(Box::new(ctx));
	let s=unsafe{
		std::slice::from_raw_parts(ptr as *const _ as *const u8, std::mem::size_of::<md5::Context>())
	};
	use base64::Engine;
	let s=base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
	unsafe{
		let _=Box::from_raw(ptr);
	}
	s
}
pub(crate) fn md5_ontext_from_raw(s:&String)->md5::Context{
	use base64::Engine;
	let raw=base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap();
	let s = unsafe {
		Box::from_raw(raw.leak() as * mut _ as *mut md5::Context)
	};
	*s
}
impl Context{
	pub async fn upload_session(&mut self,authorization: Option<&axum::http::HeaderValue>,del:bool)->Result<(UploadSession,String),Response>{
		let session=match authorization.map(|v|v.to_str().map(|s|{
			if s.starts_with("Bearer "){
				Some(&s["Bearer ".len()..])
			}else{
				None
			}
		})){
			Some(Ok(Some(session_id)))=>{
				let sid={
					use sha2::{Sha256, Digest};
					let mut hasher = Sha256::new();
					hasher.update(session_id.as_bytes());
					let hash=hasher.finalize();
					use base64::Engine;
					base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
				};
				use redis::AsyncCommands;
				let res=if del{
					self.redis.get_del::<&String,String>(&sid).await.map(|v|serde_json::from_str::<UploadSession>(&v))
				}else{
					self.redis.get::<&String,String>(&sid).await.map(|v|serde_json::from_str::<UploadSession>(&v))
				};
				match res{
					Ok(Ok(s))=>Ok((s,sid)),
					Ok(Err(_))=>{
						let mut header=axum::http::header::HeaderMap::new();
						self.config.set_cors_header(&mut header);
						return Err((StatusCode::INTERNAL_SERVER_ERROR,header).into_response())
					},
					_=>{
						let mut header=axum::http::header::HeaderMap::new();
						self.config.set_cors_header(&mut header);
						return Err((StatusCode::FORBIDDEN,header).into_response())
					},
				}
			},
			e=>{
				eprintln!("{}:{} {:?}",file!(),line!(),e);
				let mut header=axum::http::header::HeaderMap::new();
				self.config.set_cors_header(&mut header);
				return Err((StatusCode::BAD_REQUEST,header).into_response())
			}
		};
		session
	}
}
