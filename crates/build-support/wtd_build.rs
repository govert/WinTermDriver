use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

const STATE_FILE: &str = ".wtd-build-state";
const LOCK_FILE: &str = ".wtd-build-state.lock";

pub fn emit_wtd_build_metadata() {
    let repo_root = repo_root();
    let state_path = repo_root.join(STATE_FILE);

    println!("cargo:rerun-if-changed={}", state_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join(".git/HEAD").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join(".git/refs/heads/master").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join(".git/packed-refs").display()
    );

    let invocation_key = cargo_invocation_key();
    let build_number = reserve_build_number(&repo_root, &state_path, &invocation_key);
    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());
    let build_version = build_version(&package_version, build_number);
    let git_sha = git_output(&repo_root, &["rev-parse", "--short=7", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .current_dir(&repo_root)
        .args(["diff", "--quiet"])
        .status()
        .ok()
        .map(|status| !status.success())
        .unwrap_or(false);

    println!("cargo:rustc-env=WTD_VERSION={build_version}");
    println!("cargo:rustc-env=WTD_BUILD_NUMBER={build_number}");
    println!("cargo:rustc-env=WTD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=WTD_GIT_DIRTY={dirty}");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is under <repo>/crates/<name>")
        .to_path_buf()
}

fn reserve_build_number(repo_root: &Path, state_path: &Path, invocation_key: &str) -> u64 {
    let _lock = BuildLock::acquire(&repo_root.join(LOCK_FILE)).expect("failed to lock build state");
    let mut state = BuildState::read(state_path);

    if state.invocation_key == invocation_key && state.build_number > 0 {
        return state.build_number;
    }

    let floor = git_output(repo_root, &["rev-list", "--count", "HEAD"])
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    state.build_number = state.build_number.max(floor) + 1;
    state.invocation_key = invocation_key.to_string();
    state
        .write(state_path)
        .expect("failed to write build state");
    state.build_number
}

fn build_version(package_version: &str, build_number: u64) -> String {
    let base = package_version
        .split_once('-')
        .map(|(base, _)| base)
        .unwrap_or(package_version)
        .split_once('+')
        .map(|(base, _)| base)
        .unwrap_or(package_version);
    let mut parts = base.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    format!("{major}.{minor}.{build_number}")
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn cargo_invocation_key() -> String {
    let parent =
        parent_process_id().unwrap_or_else(|| format!("build-script-{}", std::process::id()));
    let makeflags = env::var("CARGO_MAKEFLAGS").unwrap_or_default();
    format!("parent={parent};makeflags={makeflags}")
}

#[cfg(windows)]
fn parent_process_id() -> Option<String> {
    let script = format!(
        "(Get-CimInstance Win32_Process -Filter \"ProcessId={}\").ParentProcessId",
        std::process::id()
    );
    Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(not(windows))]
fn parent_process_id() -> Option<String> {
    None
}

#[derive(Default)]
struct BuildState {
    build_number: u64,
    invocation_key: String,
}

impl BuildState {
    fn read(path: &Path) -> Self {
        let mut state = Self::default();
        let Ok(contents) = fs::read_to_string(path) else {
            return state;
        };
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "build_number" => {
                    state.build_number = value.parse().unwrap_or(0);
                }
                "invocation_key" => {
                    state.invocation_key = value.to_string();
                }
                _ => {}
            }
        }
        state
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(
            path,
            format!(
                "build_number={}\ninvocation_key={}\n",
                self.build_number, self.invocation_key
            ),
        )
    }
}

struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        for _ in 0..200 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale_lock(path) {
                        let _ = fs::remove_file(path);
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => return Err(err),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out waiting for {}", path.display()),
        ))
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age > Duration::from_secs(30))
        .unwrap_or(false)
}
