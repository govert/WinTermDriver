//! RAII wrapper for Win32 HANDLE.

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// RAII wrapper that closes a Win32 HANDLE on drop.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// SAFETY: OwnedHandle has exclusive ownership of the HANDLE.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}
