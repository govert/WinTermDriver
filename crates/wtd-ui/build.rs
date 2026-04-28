#[cfg(windows)]
fn main() {
    winresource::WindowsResource::new()
        .set_icon("../../assets/wtd.ico")
        .set("FileDescription", "WinTermDriver UI")
        .compile()
        .expect("failed to embed Windows resources");
}

#[cfg(not(windows))]
fn main() {}
