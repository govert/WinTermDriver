#![cfg(windows)]

use wtd_pty::ScreenBuffer;

#[test]
fn kitty_inline_image_bytes_create_placeholder_state() {
    let mut screen = ScreenBuffer::new(80, 24, 0);
    screen.advance(b"\x1b_Gi=1,a=q,t=d,f=100;wtd-probe\x1b\\");

    let image = screen
        .inline_image_at(0, 0)
        .expect("inline image placeholder");
    assert_eq!(image.protocol, "kitty");
    assert!(image.params.contains("i=1"));
    assert_eq!(image.payload, "wtd-probe");
    assert_eq!(screen.cell(0, 0).unwrap().text.as_str(), "▣");
}
