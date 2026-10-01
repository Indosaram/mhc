use maho_ext_api::{AbortSignal, EventBus, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo, ToolContent};
use maho_ext_video_in::{VideoIn, read_video, detect_video_mime_type, model_supports_video};
use std::path::Path;

#[test]
fn tool_registered() {
    let mut api=ExtensionApi::new(LoadedExtension::new("video", "/tmp".into(), SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    VideoIn.register(&mut api);
    assert_eq!(api.registered.tools[0].definition.name,"read_video");
}
#[test]
fn unsupported_model() { assert!(!model_supports_video(None)); }
#[tokio::test]
async fn unsupported_extension() {
    let error=read_video(Path::new("notes.txt"),&AbortSignal::default()).await.expect_err("unsupported extension");
    assert!(matches!(error,maho_tools::definition::ToolError::Message(_)));
}
#[tokio::test]
async fn video_attachment() {
    let dir=tempfile::tempdir().expect("tempdir"); let path=dir.path().join("clip.mp4");
    std::fs::write(&path,b"fake-mp4-bytes").expect("write fixture");
    let result=read_video(&path,&AbortSignal::default()).await.expect("read video");
    assert_eq!(result.content[1],ToolContent::Image{data:"ZmFrZS1tcDQtYnl0ZXM=".into(),mime_type:"video/mp4".into()});
}
#[tokio::test]
async fn empty_rejected() {
    let dir=tempfile::tempdir().expect("tempdir"); let path=dir.path().join("empty.mp4");
    std::fs::write(&path,[]).expect("fixture");
    assert!(read_video(&path,&AbortSignal::default()).await.is_err());
}
#[test]
fn mime_case_insensitive() { assert_eq!(detect_video_mime_type(Path::new("clip.MOV")),Some("video/quicktime")); }
