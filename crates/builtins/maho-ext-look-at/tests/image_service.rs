use maho_ext_look_at::image_input::*;
#[tokio::test]
async fn loader_consumes_host_image_service_in_source_order_with_resize_options(){
    let processor:ImageProcessor=std::sync::Arc::new(|bytes,mime,options|Box::pin(async move{
        assert!(options.auto_resize_images);assert_eq!(mime,"image/png");assert_eq!(&bytes[..8],b"\x89PNG\r\n\x1a\n");
        Ok(ProcessedImage{data:"processed".into(),mime_type:"image/webp".into(),hints:vec!["host resize".into()]})
    }));
    let ctx=LookAtImageInputContext{cwd:std::path::Path::new("/tmp"),branch:&[],auto_resize:true,block_images:false};
    let inputs=load_look_at_inputs_with_processor(&ctx,&[],&["data:image/png;base64,iVBORw0KGgo=".into(),"data:audio/wav;base64,YQ==".into()],Some(&processor)).await.unwrap();
    assert_eq!(inputs.len(),2);assert_eq!(inputs[0].data,"processed");assert_eq!(inputs[0].mime_type,"image/webp");assert_eq!(inputs[1].data,"YQ==");assert_eq!(inputs[1].mime_type,"audio/wav");
}
#[tokio::test]
async fn blocked_input_does_not_call_host_processor(){
    let processor:ImageProcessor=std::sync::Arc::new(|_,_,_|panic!("blocked input must not reach processor"));
    let ctx=LookAtImageInputContext{cwd:std::path::Path::new("/tmp"),branch:&[],auto_resize:true,block_images:true};
    assert!(load_look_at_inputs_with_processor(&ctx,&[],&["data:image/png;base64,iVBORw0KGgo=".into()],Some(&processor)).await.is_err());
}
