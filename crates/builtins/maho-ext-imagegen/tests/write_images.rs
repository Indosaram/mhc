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

#[test]
fn node_buffer_base64_accepts_noise_url_alphabet_and_missing_padding() {
    for (index,data,expected) in [(0," A!Q I\nD ",vec![1,2,3]),(1,"_w",vec![255]),(2,"AQ",vec![1]),(3,"!!!",vec![])] {
        let cwd=tempfile::tempdir().expect("temp dir");
        let target=cwd.path().join(format!("{index}.png"));
        let image=GeneratedImage {data:data.into(),mime_type:"image/png".into(),revised_prompt:None};
        let outcome=write_images(std::slice::from_ref(&target),&[image]);
        let bytes=std::fs::read(&target).ok();
        cwd.close().expect("cleanup");
        assert!(outcome.is_ok(),"{data:?}: {outcome:?}");
        assert_eq!(bytes,Some(expected));
    }
}
