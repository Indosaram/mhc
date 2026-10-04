use std::{collections::HashMap, path::Path, sync::{Arc, Mutex}, time::Duration};
use maho_codemode::{kernels::js::context_manager::JavaScriptKernel, tool::{eval_tool::create_eval_tool, eval_tool_options::*, types::*, detached_cell_manager::EvalDetachedCellManager, image_resize::*}};
use maho_tools::definition::{ToolCall, AbortSignal, ToolContext, ToolSessionManager, ToolContent};
use serde_json::json;

struct Manager(Arc<JavaScriptKernel>);
impl EvalKernelManager for Manager {
    fn get_kernel(&self, _: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> { Box::pin(async { Ok(self.0.clone() as Arc<dyn EvalKernel>) }) }
}
struct Context(maho_ai::model::Model);
impl ToolSessionManager for Context {
    fn session_id(&self) -> &str { "image-context" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl ToolContext for Context {
    fn cwd(&self) -> &Path { Path::new(env!("CARGO_MANIFEST_DIR")) }
    fn model(&self) -> Option<&maho_ai::model::Model> { Some(&self.0) }
    fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> { None }
    fn session_manager(&self) -> &dyn ToolSessionManager { self }
    fn goal_store_file(&self) -> Option<&Path> { None }
}
struct Images;
impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, bytes: Vec<u8>, mime: &'a str, _: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> {
        Box::pin(async move { assert_eq!(bytes, [1,2,3]); assert_eq!(mime,"image/webp"); Ok(None) })
    }
    fn convert_to_png<'a>(&'a self, data: &'a str, mime: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> {
        Box::pin(async move { assert_eq!(data,"AQID"); assert_eq!(mime,"image/webp"); Ok(Some(EvalImageContent {data:"png-fixture".into(),mime_type:"image/png".into()})) })
    }
}
struct Executor;
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for Executor {
    fn execute_tool<'a>(&'a self, _: &'a str, _: serde_json::Value, _: maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> { Box::pin(async { panic!("display must not call host tools") }) }
}

#[tokio::test]
async fn callable_eval_uses_each_invocations_model_for_display_images() {
    let kernel=Arc::new(JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"image-context",4,None).await.unwrap());
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(Manager(kernel.clone())),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let tool=create_eval_tool(options).unwrap();
    let mut results=Vec::new();
    for (id,provider,api,expected) in [("provider","ollama","openai-responses","image/png"),("api","other","ollama-chat","image/png"),("normal","openai","openai-responses","image/webp")] {
        let context=Context(serde_json::from_value(json!({"id":id,"name":id,"provider":provider,"api":api,"baseUrl":"http://localhost","reasoning":false,"input":["text","image"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":100})).unwrap());
        let result=tokio::time::timeout(Duration::from_secs(10),(tool.execute)(ToolCall {id,params:json!({"language":"js","code":"display({type:'image',mimeType:'image/webp',data:'AQID'})","summary":"display image","on_timeout":"error"}),signal:AbortSignal::default(),on_update:None,context:Some(&context)})).await;
        results.push((result,expected));
    }
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
    eprintln!("cleanup: model-context worker closed; pid None");
    for (result,expected) in results {
        let result=result.unwrap().unwrap();
        let image=result.content.iter().find_map(|content|match content {ToolContent::Image {mime_type,..}=>Some(mime_type.as_str()),_=>None});
        assert_eq!(image,Some(expected),"{result:?}");
    }
}
