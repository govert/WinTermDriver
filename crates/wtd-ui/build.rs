use std::process::Command;

fn main() {
    embed_windows_resources();
    emit_build_metadata();
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

fn emit_build_metadata() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads/master");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .ok()
        .map(|status| !status.success())
        .unwrap_or(false);

    println!("cargo:rustc-env=WTD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=WTD_GIT_DIRTY={dirty}");
}
