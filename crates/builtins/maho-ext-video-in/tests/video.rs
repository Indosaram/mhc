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
fn video_model()->maho_ext_api::Model{serde_json::from_value(serde_json::json!({"id":"video","name":"video","provider":"faux","api":"faux","baseUrl":"","reasoning":false,"input":["video"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).expect("model")}
#[test]
fn enables_video(){let active=vec!["read".into()];assert_eq!(maho_ext_video_in::activation_change(&active,Some(&video_model())),Some(vec!["read".into(),"read_video".into()]));}
#[test]
fn disables_video(){assert_eq!(maho_ext_video_in::activation_change(&["read_video".into(),"read".into()],None),Some(vec!["read".into()]));}
#[test]
fn unchanged_activation(){assert!(maho_ext_video_in::activation_change(&["read_video".into()],Some(&video_model())).is_none());assert!(maho_ext_video_in::activation_change(&["read".into()],None).is_none());}
