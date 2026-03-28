//! Windows Job Object for process tree management (§14.6).
//!
//! The host creates one `JobObject` per workspace instance and adds every
//! child process to it.  The `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag
//! ensures that if the host exits unexpectedly, all child processes in the
//! workspace are terminated automatically.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use crate::error::PtyError;
use crate::handle::OwnedHandle;

/// RAII wrapper around a Windows Job Object.
///
/// Configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so that all member
/// processes are terminated when this handle is closed (or the host exits).
pub struct JobObject {
    handle: OwnedHandle,
}

// SAFETY: JobObject has exclusive ownership of its HANDLE.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// Create a new anonymous Job Object with kill-on-close semantics.
    pub fn new() -> Result<Self, PtyError> {
        let h = unsafe { CreateJobObjectW(None, None) }
            .map_err(|e| PtyError::JobObject(e.to_string()))?;

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        unsafe {
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| PtyError::JobObject(e.to_string()))?;
        }

        Ok(Self { handle: OwnedHandle(h) })
    }

    /// Add a process (identified by its open `HANDLE`) to this Job Object.
    pub fn add_process(&self, process_handle: HANDLE) -> Result<(), PtyError> {
        unsafe {
            AssignProcessToJobObject(self.handle.0, process_handle)
                .map_err(|e| PtyError::JobObject(e.to_string()))?;
        }
        Ok(())
    }
}
