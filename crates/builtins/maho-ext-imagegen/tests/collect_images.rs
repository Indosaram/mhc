use maho_ai::types::{ContentBlock,ImageContent,TextContent};
use maho_ext_imagegen::tool::collect_images;
fn text(value:&str)->ContentBlock{ContentBlock::Text(TextContent{text:value.into(),..Default::default()})}
fn image(value:&str)->ContentBlock{ContentBlock::Image(ImageContent{data:value.into(),mime_type:"image/png".into()})}
#[test]
fn revised_text_attaches_only_to_next_image(){
    let images=collect_images(&[text(" first "),image("YQ=="),image("Yg=="),text("unused"),text(" last "),image("Yw==")]);
    assert_eq!(images.len(),3);
    assert_eq!(images[0].revised_prompt.as_deref(),Some("first"));
    assert_eq!(images[1].revised_prompt,None);
    assert_eq!(images[2].revised_prompt.as_deref(),Some("last"));
    assert_eq!(images[2].data,"Yw==");
}
#[test]
fn empty_text_clears_prior_revision_and_text_only_output_has_no_images(){
    let images=collect_images(&[text("discard"),text(" \n "),image("YQ==")]);
    assert_eq!(images[0].revised_prompt,None);
    assert!(collect_images(&[text("no image")]).is_empty());
}
