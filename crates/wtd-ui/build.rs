#[path = "../build-support/wtd_build.rs"]
mod wtd_build;

fn main() {
    embed_windows_resources();
    wtd_build::emit_wtd_build_metadata();
}

#[cfg(windows)]
fn embed_windows_resources() {
    winresource::WindowsResource::new()
        .set_icon("../../assets/wtd.ico")
        .set("FileDescription", "WinTermDriver UI")
        .compile()
        .expect("failed to embed Windows resources");
}

#[cfg(not(windows))]
fn embed_windows_resources() {}
