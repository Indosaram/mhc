pub mod paths;
pub mod auth;
pub mod params;
pub mod state;
pub mod reference_images;
pub mod tool;

use maho_ext_api::*;
use std::{sync::Arc,path::PathBuf};
pub const IMAGE_GEN_SECTION: &str = "\n## Image Generation\n\nWhen image generation tooling is present, read the gpt-image-gen skill before generating.\nUse the image generation tool currently available in this session.\n";
pub struct ImageGen { pub skill_path: PathBuf }
impl Default for ImageGen { fn default()->Self{Self{skill_path:PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skill/SKILL.md")}} }
impl Extension for ImageGen {
    fn register(&self,api:&mut ExtensionApi){
        let mut definition=ToolDefinition::new("generate_image","Generate or edit an image with OpenAI gpt-image-2.5 (Sunburst by default, the most capable; Flare when speed matters more than quality) and save it as a png, jpeg, or webp file, optionally with a transparent background. Pass reference_image_paths to edit or reference existing images. Generate directly when the request is clear instead of asking for confirmation. Returns the saved file paths.",params::parameters(),Arc::new(|_|Box::pin(async{Err(ToolError::Message("Extension context required".into()))})));
        definition.label="Generate Image".into();definition.exposure=Some(ToolExposure::Search);definition.search_group=Some("imagegen".into());
        definition.prompt_snippet=Some("Generate or edit images from prompts and optional reference images, saving them as png/jpeg/webp files.".into());
        definition.search_keywords=Some(["generate image","image generation","edit image","create a picture","illustration","mockup","gpt-image","transparent png"].map(str::to_owned).to_vec());
        api.register_tool_with_extension_context(definition,Arc::new(|id,args,signal,_,ctx|Box::pin(async move{tool::execute_image(id,&args,signal,ctx).await}))).unwrap_or_else(|error|std::panic::panic_any(error));
        let skill=self.skill_path.clone();
        api.on(EventKind::ResourcesDiscover,Arc::new(move|_,ctx|{let skill=skill.clone();Box::pin(async move{
            if matches!(auth::resolve_context_image_gen_auth(ctx).await?,auth::ImageGenAuthResolution::Configured{..})&&skill.is_file(){
                return Ok(EventResult::ResourcesDiscover(ResourcesDiscoverResult{skill_paths:vec![skill.to_string_lossy().into_owned().into()],..Default::default()}));
            }
            Ok(EventResult::None)
        })}));
        api.on(EventKind::BeforeAgentStart,Arc::new(|event,ctx|Box::pin(async move{
            if let ExtensionEvent::BeforeAgentStart(event)=event
                && matches!(auth::resolve_context_image_gen_auth(ctx).await?,auth::ImageGenAuthResolution::Configured{..}){
                return Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult{message:None,system_prompt:Some(format!("{}\n{}",event.system_prompt,IMAGE_GEN_SECTION))}));
            }
            Ok(EventResult::None)
        })));
    }
}
