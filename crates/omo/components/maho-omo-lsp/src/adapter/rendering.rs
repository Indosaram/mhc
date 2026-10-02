#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Text {pub text:String,pub x:i32,pub y:i32}
impl Text {pub fn new(text:impl Into<String>)->Self {Self {text:text.into(),x:0,y:0}}}
impl std::fmt::Display for Text {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(&self.text)}}
pub fn truncate_to_width(value:&str,width:usize)->String {
    let units:Vec<_>=value.encode_utf16().collect();
    if units.len()<=width {return value.into();}
    if width<=3 {return String::from_utf16_lossy(&units[..width]);}
    format!("{}...",String::from_utf16_lossy(&units[..width-3]))
}
pub fn matches_key(data:&str,key:&str)->bool {match key {"escape"=>data=="\u{1b}","ctrl+c"=>data=="\u{3}",_=>false}}
