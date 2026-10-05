use std::{fs::{self,OpenOptions},io::Write,path::{Path,PathBuf}};

pub const GENERATE_IMAGE_TOOL_NAME: &str = "generate_image";
pub struct GeneratedImage { pub data: String, pub mime_type: String, pub revised_prompt: Option<String> }
pub fn collect_images(output:&[maho_ai::types::ContentBlock])->Vec<GeneratedImage>{
    let mut generated=Vec::new();let mut text=None;
    for block in output{match block{
        maho_ai::types::ContentBlock::Text(value)=>text=(!value.text.trim().is_empty()).then(||value.text.trim().to_owned()),
        maho_ai::types::ContentBlock::Image(image)=>generated.push(GeneratedImage{data:image.data.clone(),mime_type:image.mime_type.clone(),revised_prompt:text.take()}),
        _=>{}
    }}
    generated
}
fn decode_base64(data: &str) -> Vec<u8> {
    let mut bytes=Vec::new();let mut bits=0u32;let mut count=0;
    for byte in data.bytes() {
        let value=match byte {b'A'..=b'Z'=>byte-b'A',b'a'..=b'z'=>byte-b'a'+26,b'0'..=b'9'=>byte-b'0'+52,b'+'|b'-'=>62,b'/'|b'_'=>63,b'='=>break,_=>continue};
        bits=(bits<<6)|u32::from(value);count+=6;
        if count>=8 {count-=8;bytes.push((bits>>count) as u8);}
    }
    bytes
}
pub fn write_images(paths: &[PathBuf], images: &[GeneratedImage]) -> Result<(),String> {
    let mut written: Vec<&Path> = Vec::new();
    for (target,image) in paths.iter().zip(images) {
        let write = || -> Result<(),String> {
            if let Some(parent)=target.parent() { fs::create_dir_all(parent).map_err(|e|e.to_string())?; }
            let mut file=OpenOptions::new().write(true).create_new(true).open(target).map_err(|e|e.to_string())?;
            let bytes=decode_base64(&image.data);
            file.write_all(&bytes).map_err(|e|e.to_string())
        };
        if let Err(reason)=write() {
            for path in written { let _cleanup=fs::remove_file(path); }
            return Err(format!("Error: failed to write generated image to {}: {reason}",target.display()));
        }
        written.push(target);
    }
    Ok(())
}

use maho_ext_api::*;
use serde_json::{Value,json};
use crate::{auth::ImageGenAuthResolution,params::{GenerateImageBase,FailureReason},paths::*};

pub async fn execute_image(tool_call_id:&str,args:&Value,signal:Option<maho_ai::utils::abort::AbortSignal>,ctx:&ExtensionContext)->Result<AgentToolResult,ExtensionFailure>{
    let model_id=args["model"].as_str().unwrap_or(crate::params::DEFAULT_IMAGE_MODEL);
    let size=args["size"].as_str().unwrap_or("auto");let quality=args["quality"].as_str().unwrap_or("auto");
    let background=args["background"].as_str().unwrap_or("auto");let format=args["output_format"].as_str().unwrap_or("png");
    let requested=args["n"].as_u64().unwrap_or(1) as usize;
    let fail=|message:&str,reason,source:&str|->Result<AgentToolResult,ExtensionFailure>{
        serde_json::from_value(crate::params::failure(message,reason,GenerateImageBase{model:model_id,size,quality,background,output_format:format,requested,source})).map_err(|error|ExtensionFailure::new(error.to_string()))
    };
    let prompt=args["prompt"].as_str().unwrap_or("").trim();
    if prompt.is_empty(){return fail("Error: prompt must contain non-whitespace text.",FailureReason::InvalidParams,"none");}
    if crate::state::is_native_bypass(){return fail(crate::state::NATIVE_BYPASS_MESSAGE,FailureReason::ProviderNativeBypass,"none");}
    let auth=crate::auth::resolve_context_image_gen_auth(ctx).await?;
    let ImageGenAuthResolution::Configured{kind,api_key,base_url,headers,provenance,provider_id}=auth else{
        let ImageGenAuthResolution::None{reason}=auth else{unreachable!()};return fail(reason,FailureReason::MissingConfig,"none");
    };
    let source=if provenance=="env"{"env:OPENAI_API_KEY".into()}else{format!("{provenance}:{}",provider_id.as_deref().unwrap_or(kind))};
    let parsed=maho_ai::api::openai_images_params::parse_openai_image_size(size);
    if !parsed.ok{return fail(&format!("Error: {}",parsed.error.unwrap_or_default()),FailureReason::InvalidParams,&source);}
    let refs=args["reference_image_paths"].as_array().map(|paths|paths.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>());
    let references=match crate::reference_images::load_reference_images(&ctx.cwd,refs.as_deref()){Ok(images)=>images,Err(error)=>return fail(&error,FailureReason::InvalidParams,&source)};
    let mask=match crate::reference_images::load_mask_image(&ctx.cwd,args["mask_image_path"].as_str(),references.len()){Some(Ok(image))=>Some(image),Some(Err(error))=>return fail(&error,FailureReason::InvalidParams,&source),None=>None};
    let parsed=maho_ai::api::openai_images_params::parse_openai_image_output_options(args["background"].as_str(),Some(format),args["output_compression"].as_u64(),mask.is_some(),references.len());
    if !parsed.ok{return fail(&format!("Error: {}",parsed.error.unwrap_or_default()),FailureReason::InvalidParams,&source);}
    let output_format=match format{"jpeg"=>OutputFormat::Jpeg,"webp"=>OutputFormat::Webp,_=>OutputFormat::Png};
    let targets=match resolve_targets(&ctx.cwd,tool_call_id,requested,args["output_path"].as_str(),output_format){Ok(paths)=>paths,Err(error)=>return fail(&error,FailureReason::InvalidParams,&source)};
    for path in &targets{if Path::new(path).exists(){return fail(&format!("Error: {} already exists. Choose another output_path.",display_path(&ctx.cwd,Path::new(path))),FailureReason::InvalidParams,&source);}}
    let mut model=maho_ai::image_models::get_image_model("openai",model_id).ok_or_else(||ExtensionFailure::new("Unknown image model"))?.clone();
    model.provider=provider_id.unwrap_or_else(||"openai".into());model.base_url=base_url;model.api="openai-images".into();
    let mut options=maho_ai::types::ImagesOptions::default();options.request.api_key=Some(api_key);
    options.request.headers=Some(headers.into_iter().map(|(name,value)|(name,Some(value))).collect());
    let controller=maho_ai::utils::abort::AbortController::new();options.request.signal=Some(controller.signal());
    for (field,key) in [("size","size"),("quality","quality"),("n","n"),("background","background"),("output_format","outputFormat"),("output_compression","outputCompression"),("moderation","moderation")]{if let Some(value)=args.get(field){options.extra.insert(key.into(),value.clone());}}
    options.extra.entry("size").or_insert(json!(size));options.extra.entry("quality").or_insert(json!(quality));options.extra.entry("n").or_insert(json!(requested));options.extra.entry("outputFormat").or_insert(json!(format));
    if let Some(mask)=mask{options.extra.insert("mask".into(),serde_json::to_value(mask).map_err(|error|ExtensionFailure::new(error.to_string()))?);}
    let input=[vec![maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent{text:prompt.into(),..Default::default()})],references.into_iter().map(maho_ai::types::ContentBlock::Image).collect()].concat();
    let image_context=maho_ai::types::ImagesContext{input};
    let generation=maho_ai::images::generate_images(&model,&image_context,Some(options));
    let images=tokio::select!{
        biased;
        ()=async{if let Some(signal)=signal{signal.cancelled().await}else{std::future::pending().await}}=>{controller.abort(None);return fail("Error: Image generation aborted.",FailureReason::ProviderError,&source);}
        result=generation=>result.map_err(|error|ExtensionFailure::new(error.to_string()))?,
    };
    if images.stop_reason!=maho_ai::types::ImagesStopReason::Stop{return fail(&format!("Error: {}",images.error_message.as_deref().unwrap_or("Image generation failed.")),FailureReason::ProviderError,&source);}
    let generated=collect_images(&images.output);
    if generated.is_empty(){return fail("Error: the provider returned no images.",FailureReason::ProviderError,&source);}
    let delivered=generated.iter().map(|image|output_format_of(&image.mime_type).unwrap_or(output_format)).collect::<Vec<_>>();
    let paths=targets.iter().enumerate().map(|(index,path)|delivered.get(index).filter(|format|**format!=output_format).map_or_else(||path.clone(),|format|with_format_extension(path,*format))).collect::<Vec<_>>();
    for (index,path) in paths.iter().enumerate(){if path!=&targets[index]&&Path::new(path).exists(){return fail(&format!("Error: the provider returned a different format and {} already exists. Choose another output_path.",display_path(&ctx.cwd,Path::new(path))),FailureReason::WriteFailed,&source);}}
    if let Err(error)=write_images(&paths.iter().map(PathBuf::from).collect::<Vec<_>>(),&generated){return fail(&error,FailureReason::WriteFailed,&source);}
    let saved=paths.iter().take(generated.len()).map(|path|display_path(&ctx.cwd,Path::new(path))).collect::<Vec<_>>();
    let revised=generated.iter().filter_map(|image|image.revised_prompt.clone()).collect::<Vec<_>>();
    let saved_format=match delivered[0]{OutputFormat::Png=>"png",OutputFormat::Jpeg=>"jpeg",OutputFormat::Webp=>"webp"};
    let mut details=json!({"paths":saved,"model":model_id,"source":source,"size":size,"quality":quality,"background":background,"outputFormat":saved_format,"requested":requested,"generated":generated.len(),"revisedPrompts":revised});
    if let Some(background)=images.background{details["transparentBackground"]=json!(background==maho_ai::types::ImagesBackground::Transparent);}
    let mut summary=vec![format!("Generated {} image{}:",generated.len(),if generated.len()==1{""}else{"s"})];
    summary.extend(saved.iter().map(|path|format!("- {path}")));
    if saved_format!=format{summary.push(format!("Note: the provider returned {saved_format} instead of the requested {format}; saved with the matching extension."));}
    if let Some(background)=images.background{summary.push(format!("Background: {}",if background==maho_ai::types::ImagesBackground::Transparent{"transparent"}else{"opaque"}));}
    summary.extend(revised.iter().map(|prompt|format!("Revised prompt: {prompt}")));
    summary.push("The saved file is the deliverable; refer to it by path instead of re-embedding image data.".into());
    let summary=summary.join("\n");
    let mut content=vec![maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent{text:summary,..Default::default()})];content.extend(generated.into_iter().map(|image|maho_ai::types::ContentBlock::Image(maho_ai::types::ImageContent{data:image.data,mime_type:image.mime_type})));
    Ok(AgentToolResult{content,details,usage:images.usage,added_tool_names:None,terminate:None,is_error:None})
}
