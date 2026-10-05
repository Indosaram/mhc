use maho_server::protocol::framing::{FrameDecoder, encode_frame};

#[test]
fn prefixes_payload_with_big_endian_length() {
    let payload = [0xaa, 0xbb, 0xcc];
    let frame = encode_frame(&payload).unwrap();
    assert_eq!(frame, [0, 0, 0, 3, 0xaa, 0xbb, 0xcc]);
    assert_eq!(encode_frame(&[]).unwrap(), [0, 0, 0, 0]);
}
#[test]
fn decodes_fragmented_coalesced_and_empty_frames() {
    let wire = [
        encode_frame(&[1, 2, 3]).unwrap(),
        encode_frame(&[]).unwrap(),
        encode_frame(&[4]).unwrap(),
    ]
    .concat();
    let mut decoder = FrameDecoder::default();
    let frames: Vec<_> = wire
        .iter()
        .flat_map(|b| decoder.push(&[*b]).unwrap())
        .collect();
    decoder.end().unwrap();
    assert_eq!(frames, vec![vec![1, 2, 3], vec![], vec![4]]);
    let mut decoder = FrameDecoder::default();
    assert_eq!(decoder.push(&wire).unwrap(), frames);
    decoder.end().unwrap();
}
#[test]
fn assembles_payload_spanning_internal_blocks() {
    let payload: Vec<_> = (0..70_000)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let wire = encode_frame(&payload).unwrap();
    let mut decoder = FrameDecoder::default();
    let frames = [
        decoder.push(&wire[..101]).unwrap(),
        decoder.push(&wire[101..65_541]).unwrap(),
        decoder.push(&wire[65_541..]).unwrap(),
    ]
    .concat();
    decoder.end().unwrap();
    assert_eq!(frames, vec![payload]);
}
#[test]
fn handles_every_frame_split_point() {
    let wire = encode_frame(&[10, 20, 30, 40]).unwrap();
    for split in 0..=wire.len() {
        let mut decoder = FrameDecoder::default();
        let frames = [
            decoder.push(&wire[..split]).unwrap(),
            decoder.push(&wire[split..]).unwrap(),
        ]
        .concat();
        decoder.end().unwrap();
        assert_eq!(frames, vec![vec![10, 20, 30, 40]]);
    }
}
#[test]
fn copies_payload_without_aliasing_input() {
    let mut chunk = encode_frame(&[1, 2, 3]).unwrap();
    let mut decoder = FrameDecoder::default();
    let frames = decoder.push(&chunk).unwrap();
    chunk.fill(9);
    assert_eq!(frames, vec![vec![1, 2, 3]]);
}
#[test]
fn accepts_empty_chunk_and_stream() {
    let mut decoder = FrameDecoder::default();
    assert!(decoder.push(&[]).unwrap().is_empty());
    decoder.end().unwrap();
}
#[test]
fn rejects_partial_header_at_end() {
    let mut decoder = FrameDecoder::default();
    assert!(decoder.push(&[0, 0, 0]).unwrap().is_empty());
    assert_eq!(
        decoder.end().unwrap_err().to_string(),
        "Truncated frame at end of stream"
    );
}
#[test]
fn rejects_partial_payload_at_end() {
    let mut decoder = FrameDecoder::default();
    assert!(decoder.push(&[0, 0, 0, 2, 1]).unwrap().is_empty());
    assert!(decoder.end().is_err());
}
#[test]
fn rejects_oversized_header_and_latches_failure() {
    let mut decoder = FrameDecoder::new(3);
    assert!(
        decoder
            .push(&[0, 0, 0, 4])
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    assert_eq!(
        decoder.push(&[1]).unwrap_err().to_string(),
        "Frame decoder has failed"
    );
}
#[test]
fn accepts_exact_maximum_payload() {
    let mut decoder = FrameDecoder::new(3);
    assert_eq!(
        decoder.push(&encode_frame(&[1, 2, 3]).unwrap()).unwrap(),
        vec![vec![1, 2, 3]]
    );
    decoder.end().unwrap();
}
#[test]
fn cannot_push_or_end_after_end() {
    let mut decoder = FrameDecoder::default();
    decoder.end().unwrap();
    assert_eq!(
        decoder.push(&[]).unwrap_err().to_string(),
        "Frame decoder has ended"
    );
    assert!(decoder.end().is_err());
}
