fn js_whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
fn normalize_seek_line(line:&str)->String {
    line.trim_matches(js_whitespace).chars().map(|c| match c {
        '‐'|'‑'|'‒'|'–'|'—'|'―'|'−'=>'-',
        '‘'|'’'|'‚'|'‛'=>'\'',
        '“'|'”'|'„'|'‟'=>'"',
        '\u{00a0}'|'\u{2002}'..='\u{200a}'|'\u{202f}'|'\u{205f}'|'\u{3000}'=>' ',
        c=>c,
    }).collect()
}
pub fn seek_sequence_with_fuzz(lines:&[String],pattern:&[String],start:usize,eof:bool)->Option<(usize,u32)> {
    if pattern.is_empty() { return Some((start,0)); }
    let last=lines.len().checked_sub(pattern.len())?;
    let start=if eof { last } else { start };
    for fuzz in [0,1,100,10000] {
        for index in start..=last {
            if lines[index..index+pattern.len()].iter().zip(pattern).all(|(line,expected)| match fuzz {
                0=>line==expected,
                1=>line.trim_end_matches(js_whitespace)==expected.trim_end_matches(js_whitespace),
                100=>line.trim_matches(js_whitespace)==expected.trim_matches(js_whitespace),
                _=>normalize_seek_line(line)==normalize_seek_line(expected),
            }) { return Some((index,fuzz)); }
        }
    }
    None
}
pub fn seek_sequence(lines:&[String],pattern:&[String],start:usize,eof:bool)->Option<usize> { seek_sequence_with_fuzz(lines,pattern,start,eof).map(|(index,_)|index) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exact_before_fuzzy() { assert_eq!(seek_sequence_with_fuzz(&[" x".into(),"x".into()],&["x".into()],0,false),Some((1,0))); }
    #[test] fn trim_end() { assert_eq!(seek_sequence_with_fuzz(&["x ".into()],&["x".into()],0,false),Some((0,1))); }
    #[test] fn fuzz_trims_bom_but_not_next_line_character() { assert_eq!(seek_sequence_with_fuzz(&["x\u{feff}".into()],&["x".into()],0,false),Some((0,1))); assert_eq!(seek_sequence(&["x\u{0085}".into()],&["x".into()],0,false),None); }
    #[test] fn trim_both() { assert_eq!(seek_sequence_with_fuzz(&[" x".into()],&["x".into()],0,false),Some((0,100))); }
    #[test] fn unicode_fuzz() { assert_eq!(seek_sequence_with_fuzz(&["a—b".into()],&["a-b".into()],0,false),Some((0,10000))); }
    #[test] fn eof_only() { assert_eq!(seek_sequence(&["x".into(),"y".into()],&["x".into()],0,true),None); }
    #[test] fn empty_and_missing() { assert_eq!(seek_sequence_with_fuzz(&[],&[],7,false),Some((7,0))); assert!(seek_sequence(&[],&["x".into()],0,false).is_none()); }
}
