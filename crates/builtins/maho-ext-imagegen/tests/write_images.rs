use maho_ext_imagegen::tool::*;
fn image()->GeneratedImage { GeneratedImage { data:"AQID".into(),mime_type:"image/png".into(),revised_prompt:None } }
#[test]
fn failed_later_write_rolls_back_only_created_images() {
    let cwd=tempfile::tempdir().expect("temp dir");
    let first=cwd.path().join("first.png");let second=cwd.path().join("second.png");
    std::fs::write(&second,b"existing").expect("existing image");
    assert!(write_images(&[first.clone(),second.clone()],&[image(),image()]).is_err());
    assert!(!first.exists());assert_eq!(std::fs::read(second).expect("existing preserved"),b"existing");
}
#[test]
fn writes_corresponding_images_and_stops_at_target_count() {
    let cwd=tempfile::tempdir().expect("temp dir");let target=cwd.path().join("nested/image.png");
    write_images(std::slice::from_ref(&target),&[image(),image()]).expect("write");
    assert_eq!(std::fs::read(target).expect("written"),[1,2,3]);
}
