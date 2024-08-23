use std::sync::Arc;

use image::{DynamicImage, GenericImageView};
use tokio::io::AsyncReadExt;

use crate::ConfigFile;

#[derive(Clone,Debug)]
pub struct FileMetaService{
	model:Arc<nsfw::Model>,
}
#[derive(Default,Clone,Debug)]
pub struct FileMetaData{
	pub maybe_sensitive:Option<bool>,
	pub blurhash:Option<String>,
	pub width:u32,
	pub height:u32,
	pub thumbnail: Option<Vec<u8>>,
}
impl FileMetaService{
	pub(crate) fn new()->Self{
		let model = nsfw::create_model(std::io::Cursor::new(include_bytes!("model.onnx")));
		let model=Arc::new(model.unwrap());
		Self{
			model,
		}
	}
	pub async fn metadata(&self,img:DynamicImage,sensitive_threshold:f32,skip_sensitive_detection:bool,thumbnail_size:u32,thumbnail_quality:f32,filter:fast_image_resize::FilterType)->FileMetaData{
		let model=self.model.clone();
		let size=img.dimensions();
		let (rgba,cp) = tokio::task::spawn_blocking(move||{
			let cp=img.clone();
			let rgba=resize(img, 224, 224, fast_image_resize::FilterType::Bilinear);
			(rgba,cp)
		}).await.unwrap_or_default();
		if rgba.is_none(){
			return Default::default();
		}
		let rgba=Arc::new(rgba.unwrap());
		let detection_src=rgba.clone();
		let maybe_sensitive=if skip_sensitive_detection{
			None
		}else{
			Some(tokio::task::spawn_blocking(move||{
				match examine(&model,detection_src){
					Ok(res)=>{
						let mut sensitive=false;
						for cat in res{
							if match cat.metric{
								nsfw::model::Metric::Drawings => false,//絵画
								nsfw::model::Metric::Neutral => false,//自然物
								nsfw::model::Metric::Hentai => cat.score>sensitive_threshold,
								nsfw::model::Metric::Porn => cat.score>sensitive_threshold,
								nsfw::model::Metric::Sexy => cat.score>sensitive_threshold,
							}{
								sensitive=true;
							}
						}
						Some(sensitive)
					},
					_=>None,
				}
			}))
		};
		let (maybe_sensitive,blurhash,thumbnail)=futures_util::join!(async{
			match maybe_sensitive{
				Some(job)=>job.await.unwrap_or_default(),
				None=>None
			}
		},tokio::task::spawn_blocking(move||{
			blurhash::encode(5,5,rgba.width(),rgba.height(),rgba.as_raw()).ok()
		}),tokio::task::spawn_blocking(move||{
			let size=cp.dimensions();
			let rgba=resize(cp, thumbnail_size.min(size.0), thumbnail_size.min(size.1), filter)?;
			let size=rgba.dimensions();
			let binding = rgba.into_raw();
			let encoder=webp::Encoder::from_rgba(&binding,size.0,size.1);
			let mem=encoder.encode_simple(false,thumbnail_quality).ok()?;
			Some(mem.to_vec())
		}));
		//misskeyでは5,5で生成してたから変えない
		FileMetaData{
			maybe_sensitive,
			blurhash:blurhash.ok().unwrap_or_default(),
			width:size.0,
			height:size.1,
			thumbnail: thumbnail.unwrap_or_default(),//todo 生成する
		}
	}
	pub async fn ffmpeg_metadata(
		&self,
		config:&ConfigFile,
		access_key:&String,
		thumbnail_size:u32,
		sensitive_threshold:f32,
		skip_sensitive_detection:bool,
	)->Option<FileMetaData>{
		let ffmpeg=config.ffmpeg.as_ref()?;
		if ffmpeg.is_empty(){
			return None;
		}
		let url=format!("{}{}",config.ffmpeg_base_url.as_ref().unwrap_or(&config.public_base_url),access_key);
		if let Ok(mut process)=tokio::process::Command::new(ffmpeg).stdout(std::process::Stdio::piped()).args(["-loglevel","quiet","-i",url.as_str(),"-frames:v","1","-f","image2pipe","-"]).spawn(){
			if let Some(mut stdout)=process.stdout.take(){
				let mut img=vec![];
				if let Err(e)=stdout.read_to_end(&mut img).await{
					println!("{:?}",e);
				}else{
					if let Ok(img)=image::load_from_memory(&img){
						let info=self.metadata(img,sensitive_threshold,skip_sensitive_detection, thumbnail_size,config.thumbnail_quality,config.thumbnail_filter.into()).await;
						let _=process.start_kill();
						return Some(info);
					}
				}
			}
			let _=process.wait().await;
		}
		None
	}

}
fn examine(
    model: &nsfw::Model,
    resized: Arc<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>>,
) -> Result<Vec<nsfw::model::Classification>, Box<dyn std::error::Error>> {
	use tract_data::tvec;
	use tract_data::prelude::Tensor;
	let image: Tensor = ndarray::Array4::from_shape_fn((1, resized.width() as usize,resized.height() as usize, 3), |(_, y, x, c)| {
		resized[(x as _, y as _)][c] as f32 / 255.0
	})
	.into();

	let result = model.run(tvec!(image.into()))?;
	let data = result[0].to_array_view::<f32>()?;

	Ok(data
		.into_iter()
		.enumerate()
		.map(|(metric, score)| nsfw::model::Classification {
			metric: metric
				.try_into()
				.expect("received invalid metric from model, this should never happen"),
			score: *score,
		})
		.collect::<Vec<_>>())
}

pub fn resize(img:DynamicImage,max_width:u32,max_height:u32,filter:fast_image_resize::FilterType)->Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>>{
	let scale = f32::min(max_width as f32 / img.width() as f32,max_height as f32 / img.height() as f32);
	let dst_width=1.max((img.width() as f32 * scale).round() as u32);
	let dst_height=1.max((img.height() as f32 * scale).round() as u32);
	use std::num::NonZeroU32;
	let width=NonZeroU32::new(img.width())?;
	let height=NonZeroU32::new(img.height())?;
	let src_image=fast_image_resize::Image::from_vec_u8(width,height,img.into_rgba8().into_raw(),fast_image_resize::PixelType::U8x4);
	let mut src_image=src_image.ok()?;
	let alpha_mul_div=fast_image_resize::MulDiv::default();
	alpha_mul_div.multiply_alpha_inplace(&mut src_image.view_mut()).ok()?;
	let dst_width=NonZeroU32::new(dst_width)?;
	let dst_height=NonZeroU32::new(dst_height)?;
	let mut dst_image = fast_image_resize::Image::new(dst_width,dst_height,src_image.pixel_type());
	let mut dst_view = dst_image.view_mut();
	let mut resizer = fast_image_resize::Resizer::new(
		fast_image_resize::ResizeAlg::Convolution(filter),
	);
	resizer.resize(&src_image.view(), &mut dst_view).ok()?;
	alpha_mul_div.divide_alpha_inplace(&mut dst_view).ok()?;
	let rgba=image::RgbaImage::from_raw(dst_image.width().get(),dst_image.height().get(),dst_image.into_vec());
	rgba
}
