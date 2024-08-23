use core::str;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ConfigFile;

#[derive(Clone,Debug)]
pub struct BackendService{
	client:reqwest::Client,
	config:Arc<ConfigFile>,
}
impl BackendService{
	pub fn new(config:Arc<ConfigFile>)->Self{
		Self{
			client:reqwest::Client::new(),
			config,
		}
	}
	pub async fn preflight(&self,req:&mut PreflightRequest)->Result<PreflightResponse,String>{
		req.upload_service_key=Some(self.config.backend.key.clone());
		let post=self.client.post(format!("{}/drive/files/upload-preflight",self.config.backend.endpoint));
		let post=post.header(reqwest::header::CONTENT_TYPE,"application/json");
		let post=post.body(serde_json::to_string(&req).unwrap());
		let res=match post.send().await{
			Ok(v)=>v,
			Err(e)=>{
				eprintln!("preflight send error {:?}",e);
				return Err("{}".to_owned());
			}
		};
		let res=res.bytes().await.map_err(|e|{
			format!("{{\"error\":{{\"message\":\"{}\"}} }}",e.to_string())
		})?;
		let res=match serde_json::from_slice(&res){
			Ok(v)=>Ok(v),
			Err(e)=>{
				eprintln!("preflight parse error {:?}\n{:?}",e,str::from_utf8(&res));
				Err(str::from_utf8(&res).map_err(|_e|"{}".to_owned())?.to_owned())
			}
		};
		res
	}
	pub async fn register(&self,req:&mut RegisterRequest)->Result<(u16,String),String>{
		req.upload_service_key=Some(self.config.backend.key.clone());
		let post=self.client.post(format!("{}/drive/files/upload-service",self.config.backend.endpoint));
		let post=post.header(reqwest::header::CONTENT_TYPE,"application/json");
		let post=post.body(serde_json::to_string(&req).unwrap());
		let res=match post.send().await{
			Ok(v)=>v,
			Err(e)=>{
				eprintln!("register send error {:?}",e);
				return Err("{}".to_owned());
			}
		};
		let status=res.status().as_u16();
		let res=res.bytes().await.map_err(|e|{
			format!("{{\"error\":{{\"message\":\"{}\"}} }}",e.to_string())
		})?;
		let res=match String::from_utf8(res.into()){
			Ok(v)=>Ok((status,v)),
			Err(e)=>{
				eprintln!("register parse error {:?}",e);
				Err(format!("register parse error {:?}",e))
			}
		};
		res
	}
}
#[derive(Default,Clone,Debug,Serialize,Deserialize)]
pub struct PreflightRequest{
	pub upload_service_key:Option<String>,
	#[serde(rename = "folderId")]
	pub folder_id:Option<String>,
	pub name:Option<String>,
	#[serde(rename = "isSensitive")]
	pub is_sensitive:bool,
	pub comment:Option<String>,
	pub size:u64,
	pub ext:Option<String>,
	#[serde(rename = "isLink")]
	pub is_link:bool,
	pub url:Option<String>,
	pub uri:Option<String>,
	pub i:Option<String>,
	pub user_id:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct RegisterRequest{
	pub upload_service_key:Option<String>,
	//https://s3.example.com/prefix/
	#[serde(rename = "baseUrl")]
	pub base_url:String,
	//c4f38298-2e66-43e9-8e29-7dfa29db8754.webp
	#[serde(rename = "accessKey")]
	pub access_key:String,
	//thumbnail-ae8b311e-f834-4eff-8605-6e5a7165f1c0.webp
	//サムネイルを生成できないファイルの場合None
	#[serde(rename = "thumbnailKey")]
	pub thumbnail_key:Option<String>,
	pub md5:String,
	pub blurhash:Option<String>,
	pub size:u64,
	pub width:u32,
	pub height:u32,
	#[serde(rename = "sourceUrl")]
	pub source_url:Option<String>,
	#[serde(rename = "remoteUri")]
	pub remote_uri:Option<String>,
	#[serde(rename = "isLink")]
	pub is_link:bool,
	#[serde(rename = "folderId")]
	pub folder_id:Option<String>,
	pub name:String,
	pub comment:Option<String>,
	#[serde(rename = "isSensitive")]
	pub is_sensitive:bool,
	#[serde(rename = "maybeSensitive")]
	pub maybe_sensitive:bool,
	#[serde(rename = "contentType")]
	pub content_type:String,
	pub force:bool,
	pub i:Option<String>,
	pub user_id:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PreflightResponse{
	#[serde(rename = "skipSensitiveDetection")]
	pub skip_sensitive_detection: bool,
	#[serde(rename = "sensitiveThreshold")]
	pub sensitive_threshold: f32,
	#[serde(rename = "enableSensitiveMediaDetectionForVideos")]
	pub enable_sensitive_media_detection_for_videos: bool,
	#[serde(rename = "detectedName")]
	pub detected_name: String,
}
