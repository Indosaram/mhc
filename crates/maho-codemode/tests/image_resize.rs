use maho_codemode::tool::image_resize::*;

struct ImageSdk { convert: bool }
impl EvalImageSdk for ImageSdk {
    fn resize_image<'a>(&'a self, bytes: Vec<u8>, mime_type: &'a str, max_bytes: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> {
        Box::pin(async move { assert_eq!(bytes, [1,2,3]); assert_eq!(mime_type,"image/webp"); assert_eq!(max_bytes,Some(4)); Ok(None) })
    }
    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> {
        Box::pin(async move { Ok(self.convert.then(||EvalImageContent {data:"png".into(),mime_type:"image/png".into()})) })
    }
}

#[test]
fn webp_model_rules_match_provider_and_api() {
    for provider in ["ollama","ollama-cloud","llama.cpp","lm-studio","local-server"] {assert!(webp_exclusion_for_model(Some(provider),None));}
    assert!(webp_exclusion_for_model(Some("other"),Some("ollama-chat")));
    assert!(!webp_exclusion_for_model(None,None));
    assert!(!webp_exclusion_for_model(Some("openai"),Some("responses")));
}

#[tokio::test]
async fn excluded_webp_conversion_is_required_not_silently_omitted() {
    let input = ||EvalImageContent {data:"AQID".into(),mime_type:"image/webp".into()};
    let result = resize_eval_image(input(),Some("ollama"),None,&ImageSdk {convert:true}).await.unwrap();
    assert_eq!(result.image.mime_type,"image/png");
    assert!(result.dimension_note.is_none());
    assert!(resize_eval_image(input(),Some("ollama"),None,&ImageSdk {convert:false}).await.is_err());
}
