use super::*;

#[test]
fn head_retains_bytes_and_newline_budget() {
    let mut b=Bounded::new(100,2,Retain::Head);
    b.push(b"one\ntwo\nthree\n"); b.push(b"four");
    assert_eq!(b.bytes(),b"one\ntwo\n");
    assert_eq!((b.total,b.dropped_bytes,b.dropped_lines),(18,10,1));
}
#[test]
fn tail_combines_chunks_and_counts_every_discard() {
    let mut b=Bounded::new(8,1,Retain::Tail);
    b.push(b"abc\ndef\n"); b.push(b"ghi\nj");
    assert_eq!(b.bytes(),b"ghi\nj");
    assert_eq!((b.total,b.dropped_bytes,b.dropped_lines),(13,8,2));
}
#[test]
fn zero_budgets_drop_all_and_empty_push_is_neutral() {
    for (bytes,lines) in [(0,5),(5,0),(0,0)] {
        let mut b=Bounded::new(bytes,lines,Retain::Tail);
        b.push(b"a\n"); b.push(b"");
        assert_eq!(b.bytes(),b"");
        assert_eq!((b.total,b.dropped(),b.dropped_lines),(2,2,1));
    }
}
#[test]
fn utf8_is_decoded_after_retention_and_strips_initial_bom() {
    let mut b=Bounded::new(3,10,Retain::Head);
    b.push(&[0xf0,0x9f]); b.push(&[0x98,0x80]);
    assert_eq!(b.text(),"\u{fffd}"); assert_eq!(b.dropped(),1);
    let mut b=Bounded::new(4,10,Retain::Tail);
    b.push(&[0xf0,0x9f]); b.push(&[0x98,0x80]);
    assert_eq!(b.text(),"\u{1f600}");
    b.push(b"x"); assert_eq!(b.text(),"\u{fffd}\u{fffd}\u{fffd}x");
    let mut bom=Bounded::new(8,5,Retain::Head);
    bom.push(&[0xef,0xbb,0xbf,b'x']); assert_eq!(bom.text(),"x");
}
#[test]
fn tail_limits_newlines_not_display_lines() {
    let mut b=Bounded::new(100,1,Retain::Tail);
    b.push(b"a\nb\nc"); assert_eq!(b.bytes(),b"b\nc");
    let mut b=Bounded::new(3,100,Retain::Head);
    b.push(b"abcdef"); assert_eq!(b.bytes(),b"abc");
}
