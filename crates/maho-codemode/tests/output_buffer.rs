use maho_codemode::output::streaming_output_buffer::*;

#[test]
fn head_preserves_utf8_boundaries() {
    assert_eq!(truncate_head_bytes("a한b", 3).text, "a");
    assert_eq!(truncate_head_bytes("a한b", 4).text, "a한");
}
#[test]
fn tail_preserves_utf8_boundaries() {
    assert_eq!(truncate_tail_bytes("a한b", 3).text, "b");
    assert_eq!(truncate_tail_bytes("a한b", 4).text, "한b");
}
#[test]
fn zero_budget_retains_nothing() {
    assert_eq!(truncate_head_bytes("abc", 0).bytes, 0);
    assert_eq!(truncate_tail_bytes("abc", 0).bytes, 0);
    let mut tail = TailBuffer::new(0);
    tail.append("abc");
    assert_eq!(tail.text(), "");
}
#[test]
fn untruncated_slice_retains_all() {
    assert_eq!(truncate_head_bytes("abc", 10).text, "abc");
    assert_eq!(truncate_tail_bytes("abc", 10).bytes, 3);
}
#[test]
fn tail_accumulates_and_trims() {
    let mut tail = TailBuffer::new(4);
    tail.append("abc"); tail.append("de");
    assert_eq!(tail.text(), "bcde");
    assert_eq!(tail.bytes(), 4);
}
#[test]
fn large_append_replaces_previous_tail() {
    let mut tail = TailBuffer::new(4);
    tail.append("old"); tail.append("abcdef");
    assert_eq!(tail.text(), "cdef");
}
#[test]
fn empty_append_keeps_tail() {
    let mut tail = TailBuffer::new(4);
    tail.append("abc"); tail.append("");
    assert_eq!(tail.text(), "abc");
}
#[test]
fn unicode_tail_append() {
    let mut tail = TailBuffer::new(5);
    tail.append("a한"); tail.append("bc");
    assert_eq!(tail.text(), "한bc");
}
