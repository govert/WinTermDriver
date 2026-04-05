//! Tests for length-prefixed binary framing (§13.4).

use wtd_ipc::framing::{self, LENGTH_PREFIX_SIZE, MAX_MESSAGE_SIZE};
use wtd_ipc::message::*;

#[test]
fn encode_decode_roundtrip() {
    let envelope = Envelope::new(
        "test-id",
        &Handshake {
            client_type: ClientType::Ui,
            client_version: "1.0.0".into(),
            protocol_version: 1,
        },
    );

    let frame = framing::encode(&envelope).unwrap();

    // First 4 bytes are the LE length prefix.
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(len + LENGTH_PREFIX_SIZE, frame.len());

    // Payload is valid UTF-8 JSON.
    let payload_bytes = &frame[LENGTH_PREFIX_SIZE..];
    let payload_str = std::str::from_utf8(payload_bytes).unwrap();
    let _: serde_json::Value = serde_json::from_str(payload_str).unwrap();

    // Decode back.
    let decoded = framing::decode(&frame).unwrap();
    assert_eq!(decoded, envelope);
}

#[test]
fn encode_decode_multiple_message_types() {
    // Verify framing works for different message types.
    let messages: Vec<Envelope> = vec![
        Envelope::new("id-1", &ListWorkspaces {}),
        Envelope::new("id-2", &OkResponse {}),
        Envelope::new(
            "id-3",
            &ErrorResponse {
                code: ErrorCode::InternalError,
                message: "boom".into(),
                candidates: None,
            },
        ),
        Envelope::new(
            "id-4",
            &SessionOutput {
                session_id: "s1".into(),
                data: "dGVzdA==".into(),
            },
        ),
    ];

    for envelope in &messages {
        let frame = framing::encode(envelope).unwrap();
        let decoded = framing::decode(&frame).unwrap();
        assert_eq!(&decoded, envelope);
    }
}

#[test]
fn length_prefix_is_correct() {
    let envelope = Envelope::new("x", &OkResponse {});
    let frame = framing::encode(&envelope).unwrap();

    let expected_json = serde_json::to_vec(&envelope).unwrap();
    let prefix_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;

    assert_eq!(prefix_len, expected_json.len());
    assert_eq!(&frame[LENGTH_PREFIX_SIZE..], &expected_json[..]);
}

#[test]
fn decode_frame_too_short_no_prefix() {
    let result = framing::decode(&[0u8; 2]);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("frame too short"));
}

#[test]
fn decode_frame_too_short_incomplete_payload() {
    // Length prefix says 100 bytes, but we only provide 10.
    let mut frame = Vec::new();
    frame.extend_from_slice(&100u32.to_le_bytes());
    frame.extend_from_slice(&[0u8; 10]);

    let result = framing::decode(&frame);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("frame too short"));
}

#[test]
fn oversize_message_rejected_on_encode() {
    // Create an envelope with a payload larger than 16 MiB.
    let huge_string = "x".repeat(MAX_MESSAGE_SIZE + 1);
    let envelope = Envelope::new(
        "big",
        &CaptureResult {
            text: huge_string,
            lines: 0,
            total_lines: 0,
            anchor_found: None,
            cursor: None,
            cols: 0,
            rows: 0,
            on_alternate: false,
            title: None,
            progress: None,
            mouse_mode: None,
            sgr_mouse: false,
            bracketed_paste: false,
            cursor_row: None,
            cursor_col: None,
            cursor_visible: None,
            cursor_shape: None,
        },
    );

    let result = framing::encode(&envelope);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("message too large"));
}

#[test]
fn oversize_message_rejected_on_decode() {
    // Craft a frame with a length prefix exceeding 16 MiB.
    let oversized_len = (MAX_MESSAGE_SIZE + 1) as u32;
    let mut frame = Vec::new();
    frame.extend_from_slice(&oversized_len.to_le_bytes());
    // Don't need to fill the actual payload — decode should reject at length check.
    frame.extend_from_slice(&[0u8; 8]);

    let result = framing::decode(&frame);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("message too large"));
}

#[test]
fn read_length_prefix_works() {
    let len: u32 = 42;
    let buf = len.to_le_bytes();
    assert_eq!(framing::read_length_prefix(&buf), Some(42));
}

#[test]
fn read_length_prefix_too_short() {
    assert_eq!(framing::read_length_prefix(&[0u8; 2]), None);
    assert_eq!(framing::read_length_prefix(&[]), None);
}

#[test]
fn concatenated_frames() {
    // Simulate reading two frames from a stream.
    let env1 = Envelope::new("a", &OkResponse {});
    let env2 = Envelope::new(
        "b",
        &Handshake {
            client_type: ClientType::Cli,
            client_version: "1.0".into(),
            protocol_version: 1,
        },
    );

    let frame1 = framing::encode(&env1).unwrap();
    let frame2 = framing::encode(&env2).unwrap();

    let mut stream = Vec::new();
    stream.extend_from_slice(&frame1);
    stream.extend_from_slice(&frame2);

    // Parse first frame.
    let len1 = framing::read_length_prefix(&stream).unwrap();
    let end1 = LENGTH_PREFIX_SIZE + len1;
    let decoded1 = framing::decode(&stream[..end1]).unwrap();
    assert_eq!(decoded1, env1);

    // Parse second frame.
    let remaining = &stream[end1..];
    let len2 = framing::read_length_prefix(remaining).unwrap();
    let end2 = LENGTH_PREFIX_SIZE + len2;
    let decoded2 = framing::decode(&remaining[..end2]).unwrap();
    assert_eq!(decoded2, env2);
}
