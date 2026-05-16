#[path = "../build-support/wtd_build.rs"]
mod wtd_build;

fn main() {
    wtd_build::emit_wtd_build_metadata();
}
