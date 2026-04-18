#![cfg(windows)]

use wtd_host::terminal_input::{encode_key_spec, encode_key_spec_with_protocol};
use wtd_pty::screen::KeyboardProtocolMode;

#[test]
fn kitty_protocol_emits_csi_u_for_modified_printable_keys() {
    assert_eq!(
        encode_key_spec_with_protocol("Alt+X", KeyboardProtocolMode::Kitty).unwrap(),
        b"\x1b[120;3u"
    );
    assert_eq!(
        encode_key_spec_with_protocol("Ctrl+C", KeyboardProtocolMode::Kitty).unwrap(),
        b"\x1b[99;5u"
    );
}

#[test]
fn enhanced_protocols_preserve_modified_enter_sequences() {
    assert_eq!(encode_key_spec("Shift+Enter").unwrap(), b"\x1b[13;2u");
    assert_eq!(
        encode_key_spec_with_protocol("Alt+Enter", KeyboardProtocolMode::CsiU).unwrap(),
        b"\x1b[13;3u"
    );
    assert_eq!(
        encode_key_spec_with_protocol("Ctrl+Enter", KeyboardProtocolMode::Kitty).unwrap(),
        b"\x1b[13;5u"
    );
}

#[test]
fn legacy_protocol_falls_back_for_modified_enter_but_keeps_other_modified_keys() {
    assert_eq!(
        encode_key_spec_with_protocol("Shift+Enter", KeyboardProtocolMode::Legacy).unwrap(),
        b"\r"
    );
    assert_eq!(
        encode_key_spec_with_protocol("Shift+Up", KeyboardProtocolMode::Legacy).unwrap(),
        b"\x1b[1;2A"
    );
    assert_eq!(
        encode_key_spec_with_protocol("Ctrl+F5", KeyboardProtocolMode::Legacy).unwrap(),
        b"\x1b[15;5~"
    );
}
