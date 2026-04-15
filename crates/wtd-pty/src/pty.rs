//! ConPTY pseudo-console lifecycle: create, resize, I/O, and close.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::mem;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

use crate::error::PtyError;
use crate::handle::OwnedHandle;
use crate::job::JobObject;

// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = ProcThreadAttributePseudoConsole(22) | PROC_THREAD_ATTRIBUTE_INPUT(0x20000)
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

/// Size of a pseudo-console in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// A live ConPTY session: pseudo-console + child process + I/O pipes.
///
/// Drop closes the ConPTY (signalling the child), waits briefly for a natural
/// exit, then forcibly terminates the child if it is still running.
pub struct PtySession {
    hpc: HPCON,
    process_handle: OwnedHandle,
    _thread_handle: OwnedHandle,
    input_write: OwnedHandle,
    output_read: OwnedHandle,
    /// PID of the child process.
    pub pid: u32,
}

// SAFETY: HPCON wraps isize (an opaque handle value). PtySession has exclusive ownership.
unsafe impl Send for PtySession {}
unsafe impl Sync for PtySession {}

impl PtySession {
    /// Spawn a child process inside a new pseudo-console.
    ///
    /// - `executable`: path or name of the executable (e.g. `"powershell.exe"`)
    /// - `args`: additional command-line arguments
    /// - `cwd`: working directory for the child; `None` to inherit the host's cwd
    /// - `size`: initial terminal dimensions
    /// - `env`: optional child environment block; `None` to inherit the host env
    /// - `job`: optional Job Object to add the child process to (§14.6)
    pub fn spawn(
        executable: &str,
        args: &[&str],
        cwd: Option<&str>,
        size: PtySize,
        env: Option<&HashMap<String, String>>,
        job: Option<&JobObject>,
    ) -> Result<Self, PtyError> {
        // ── 1. Create anonymous pipes ────────────────────────────────────────
        let (input_read, input_write) = create_pipe()?;
        let (output_read, output_write) = create_pipe()?;

        // ── 2. CreatePseudoConsole ───────────────────────────────────────────
        let coord = COORD {
            X: size.cols as i16,
            Y: size.rows as i16,
        };
        let hpc = unsafe {
            CreatePseudoConsole(coord, input_read.0, output_write.0, 0)
                .map_err(|e| PtyError::CreateFailed(e.code().0 as u32))?
        };

        // ── 3. Build PROC_THREAD_ATTRIBUTE_LIST with the ConPTY handle ───────
        let mut attr_buf = {
            let mut size_needed = 0usize;
            // First call always fails (ERROR_INSUFFICIENT_BUFFER) — used only to query size.
            unsafe {
                let _ = InitializeProcThreadAttributeList(
                    LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
                    1,
                    0,
                    &mut size_needed,
                );
            }
            vec![0u8; size_needed]
        };

        let attr_ptr = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);

        unsafe {
            let mut size_init = attr_buf.len();
            InitializeProcThreadAttributeList(attr_ptr, 1, 0, &mut size_init)?;

            // The C API convention for PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE:
            //   UpdateProcThreadAttribute(..., hPC, sizeof(hPC), ...)
            // where hPC is HPCON (= VOID* in C).  lpValue IS the HPCON value cast to void*;
            // Windows dereferences it to read the internal ConPTY structure identifier.
            // In windows-rs HPCON(isize) wraps that same raw pointer value as isize.
            UpdateProcThreadAttribute(
                attr_ptr,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                Some(hpc.0 as *mut std::ffi::c_void as *const _),
                mem::size_of::<HPCON>(),
                None,
                None,
            )?;

            // ── 4. Configure STARTUPINFOEXW ──────────────────────────────────
            // Set STARTF_USESTDHANDLES with INVALID_HANDLE_VALUE for all std
            // handles.  Without this, if the host has a real console, the child
            // inherits it alongside the ConPTY and its output goes to the real
            // console instead of the ConPTY pipe.  (Matches Windows Terminal's
            // ConPtyConnection.cpp approach.)
            let si_ex = STARTUPINFOEXW {
                StartupInfo: STARTUPINFOW {
                    cb: mem::size_of::<STARTUPINFOEXW>() as u32,
                    dwFlags: STARTF_USESTDHANDLES,
                    hStdInput: INVALID_HANDLE_VALUE,
                    hStdOutput: INVALID_HANDLE_VALUE,
                    hStdError: INVALID_HANDLE_VALUE,
                    ..Default::default()
                },
                lpAttributeList: attr_ptr,
            };

            // ── 5. Build wide command line ───────────────────────────────────
            let cmd_line = build_command_line(executable, args);
            let mut cmd_wide: Vec<u16> = OsStr::new(&cmd_line)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            // Optional wide cwd
            let cwd_wide: Option<Vec<u16>> = cwd.map(|c| {
                OsStr::new(c)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect()
            });
            let cwd_pcwstr = cwd_wide
                .as_ref()
                .map(|v| windows::core::PCWSTR(v.as_ptr()))
                .unwrap_or(windows::core::PCWSTR::null());
            let env_wide = env.map(build_environment_block);
            let env_ptr = env_wide
                .as_ref()
                .map(|v| v.as_ptr() as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null());

            // ── 6. CreateProcess ─────────────────────────────────────────────
            let mut pi = PROCESS_INFORMATION::default();
            CreateProcessW(
                None,
                windows::core::PWSTR(cmd_wide.as_mut_ptr()),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT
                    | if env_wide.is_some() {
                        CREATE_UNICODE_ENVIRONMENT
                    } else {
                        Default::default()
                    },
                Some(env_ptr),
                cwd_pcwstr,
                // Cast STARTUPINFOEXW* → STARTUPINFOW* (layout-compatible; cb signals EX)
                &si_ex.StartupInfo as *const STARTUPINFOW,
                &mut pi,
            )
            .map_err(|e| PtyError::SpawnFailed(e.to_string()))?;

            // Clean up attribute list
            DeleteProcThreadAttributeList(attr_ptr);

            // ── 7. Close child-side pipe ends in the host ────────────────────
            // ConPTY now owns input_read and output_write internally.
            drop(input_read);
            drop(output_write);

            // ── 8. Optionally add to Job Object ──────────────────────────────
            if let Some(j) = job {
                j.add_process(pi.hProcess)?;
            }

            Ok(PtySession {
                hpc,
                process_handle: OwnedHandle(pi.hProcess),
                _thread_handle: OwnedHandle(pi.hThread),
                input_write,
                output_read,
                pid: pi.dwProcessId,
            })
        }
    }

    /// Resize the pseudo-console to new dimensions.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        let coord = COORD {
            X: size.cols as i16,
            Y: size.rows as i16,
        };
        unsafe { ResizePseudoConsole(self.hpc, coord) }
            .map_err(|e| PtyError::ResizeFailed(e.code().0 as u32))
    }

    /// Write bytes to the child's stdin (via the ConPTY input pipe).
    pub fn write_input(&self, data: &[u8]) -> Result<(), PtyError> {
        let mut written = 0u32;
        unsafe { WriteFile(self.input_write.0, Some(data), Some(&mut written), None) }?;
        Ok(())
    }

    /// Blocking read from the child's stdout/stderr (via the ConPTY output pipe).
    pub fn read_output(&self, buf: &mut [u8]) -> Result<usize, PtyError> {
        let mut read = 0u32;
        unsafe { ReadFile(self.output_read.0, Some(buf), Some(&mut read), None) }?;
        Ok(read as usize)
    }

    /// Returns the number of bytes immediately available on the output pipe
    /// without blocking.  Returns 0 on any error (pipe not ready yet).
    pub fn peek_output_available(&self) -> u32 {
        let mut avail = 0u32;
        unsafe {
            let _ = PeekNamedPipe(self.output_read.0, None, 0, None, Some(&mut avail), None);
        }
        avail
    }

    /// Wait up to `timeout_ms` for the child process to exit.
    /// Returns `true` if the process exited within the timeout.
    pub fn wait_for_exit(&self, timeout_ms: u32) -> bool {
        let r = unsafe { WaitForSingleObject(self.process_handle.0, timeout_ms) };
        r == WAIT_OBJECT_0
    }

    /// Forcibly terminate the child process.
    pub fn terminate(&self) {
        unsafe {
            let _ = TerminateProcess(self.process_handle.0, 1);
        }
    }

    /// Returns the raw output-read pipe handle for advanced callers (e.g. async I/O).
    pub fn output_read_handle(&self) -> HANDLE {
        self.output_read.0
    }

    /// Returns the raw input-write pipe handle for advanced callers (e.g. async I/O).
    pub fn input_write_handle(&self) -> HANDLE {
        self.input_write.0
    }

    /// Returns the raw process handle for the child process.
    ///
    /// Callers can use this with `GetExitCodeProcess` or other Win32 APIs.
    pub fn process_handle(&self) -> HANDLE {
        self.process_handle.0
    }
}

fn build_environment_block(env: &HashMap<String, String>) -> Vec<u16> {
    let mut pairs: Vec<(&String, &String)> = env.iter().collect();
    pairs.sort_by(|(ka, _), (kb, _)| ka.to_ascii_uppercase().cmp(&kb.to_ascii_uppercase()));

    let mut block = Vec::new();
    for (key, value) in pairs {
        let entry = format!("{key}={value}");
        block.extend(OsStr::new(&entry).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

impl Drop for PtySession {
    fn drop(&mut self) {
        unsafe {
            // Close the ConPTY — this delivers EOF to the child process.
            ClosePseudoConsole(self.hpc);
            // Wait briefly for a natural exit before force-terminating.
            let r = WaitForSingleObject(self.process_handle.0, 1000);
            if r != WAIT_OBJECT_0 {
                let _ = TerminateProcess(self.process_handle.0, 1);
            }
            // OwnedHandle drops close the process/thread/pipe handles.
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn create_pipe() -> Result<(OwnedHandle, OwnedHandle), PtyError> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, None, 0)? };
    Ok((OwnedHandle(read), OwnedHandle(write)))
}

/// Build a quoted command-line string suitable for `CreateProcessW`.
fn build_command_line(executable: &str, args: &[&str]) -> String {
    let mut cmd = quote_arg(executable);
    for arg in args {
        cmd.push(' ');
        cmd.push_str(&quote_arg(arg));
    }
    cmd
}

/// Quote an individual argument if it contains spaces or double-quotes.
fn quote_arg(s: &str) -> String {
    if s.contains(' ') || s.contains('"') {
        let escaped = s.replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_size_fields() {
        let sz = PtySize::new(80, 24);
        assert_eq!(sz.cols, 80);
        assert_eq!(sz.rows, 24);
    }

    #[test]
    fn build_command_line_no_spaces() {
        let cmd = build_command_line("powershell.exe", &["-NoLogo"]);
        assert_eq!(cmd, "powershell.exe -NoLogo");
    }

    #[test]
    fn build_command_line_with_spaces() {
        let cmd = build_command_line("C:\\Program Files\\foo.exe", &["arg one"]);
        assert_eq!(cmd, r#""C:\Program Files\foo.exe" "arg one""#);
    }
}
