//! Named pipe security — DACL and client SID verification (spec sections 28.2, 28.3).
//!
//! Provides [`PipeSecurity`] for creating named pipes with a DACL that grants
//! access only to the owning user, and for verifying connecting clients.

use thiserror::Error;

/// Errors from pipe security operations.
#[derive(Debug, Error)]
pub enum PipeSecurityError {
    #[cfg(windows)]
    #[error("Windows API error: {0}")]
    Windows(#[from] windows::core::Error),

    #[cfg(not(windows))]
    #[error("named pipes are not supported on this platform")]
    NotSupported,
}

/// Convert raw SID binary bytes to the standard string form (`S-1-5-21-…`).
///
/// The SID binary layout is:
/// - `[0]` Revision (always 1)
/// - `[1]` SubAuthorityCount
/// - `[2..8]` IdentifierAuthority (6 bytes, big-endian)
/// - `[8..]` SubAuthority array (each 4 bytes, little-endian)
pub fn sid_to_string(sid_bytes: &[u8]) -> String {
    assert!(sid_bytes.len() >= 8, "SID buffer too short");
    let revision = sid_bytes[0];
    let sub_count = sid_bytes[1] as usize;
    let authority = &sid_bytes[2..8];

    let authority_value = if authority[0] != 0 || authority[1] != 0 {
        // Large authority — use hex form.
        format!(
            "0x{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            authority[0], authority[1], authority[2], authority[3], authority[4], authority[5],
        )
    } else {
        let val = ((authority[2] as u64) << 24)
            | ((authority[3] as u64) << 16)
            | ((authority[4] as u64) << 8)
            | (authority[5] as u64);
        val.to_string()
    };

    let mut result = format!("S-{}-{}", revision, authority_value);
    for i in 0..sub_count {
        let off = 8 + i * 4;
        let sub = u32::from_le_bytes([
            sid_bytes[off],
            sid_bytes[off + 1],
            sid_bytes[off + 2],
            sid_bytes[off + 3],
        ]);
        result.push_str(&format!("-{}", sub));
    }
    result
}

// ── Windows implementation ─────────────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::mem;
    use windows::Win32::Foundation::*;
    use windows::Win32::Security::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows::Win32::System::Threading::*;

    /// Get the current process user's SID as `(raw_bytes, string_form)`.
    pub fn get_current_user_sid() -> Result<(Vec<u8>, String), PipeSecurityError> {
        unsafe {
            let process = GetCurrentProcess();
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token)?;

            // First call to query required buffer size.
            let mut size = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);

            let mut buf = vec![0u8; size as usize];
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr() as *mut c_void),
                size,
                &mut size,
            )?;
            let _ = CloseHandle(token);

            let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
            let psid = token_user.User.Sid;
            let sid_len = GetLengthSid(psid) as usize;
            let sid_bytes = std::slice::from_raw_parts(psid.0 as *const u8, sid_len).to_vec();
            let sid_str = sid_to_string(&sid_bytes);

            Ok((sid_bytes, sid_str))
        }
    }

    /// Build the named-pipe path for the current user: `\\.\pipe\wtd-{SID}`.
    pub fn pipe_name_for_current_user() -> Result<String, PipeSecurityError> {
        let (_, sid_str) = get_current_user_sid()?;
        Ok(format!(r"\\.\pipe\wtd-{}", sid_str))
    }

    /// RAII wrapper owning the `SECURITY_DESCRIPTOR` and ACL buffers referenced
    /// by the `SECURITY_ATTRIBUTES` passed to `CreateNamedPipe`.
    pub struct PipeSecurity {
        _sd_buf: Vec<u8>,
        _acl_buf: Vec<u8>,
        sa: SECURITY_ATTRIBUTES,
        owner_sid: Vec<u8>,
    }

    // SAFETY: raw pointers in `sa` target heap-allocated Vec data owned by this
    // struct. Moving the struct moves Vec metadata, not the heap data, so the
    // pointers remain valid.
    unsafe impl Send for PipeSecurity {}
    unsafe impl Sync for PipeSecurity {}

    impl PipeSecurity {
        /// Build `SECURITY_ATTRIBUTES` granting only the current user
        /// `FILE_GENERIC_READ | FILE_GENERIC_WRITE` on the pipe.
        pub fn new() -> Result<Self, PipeSecurityError> {
            let (sid_bytes, _) = get_current_user_sid()?;

            unsafe {
                let psid = PSID(sid_bytes.as_ptr() as *mut c_void);
                let sid_len = GetLengthSid(psid);

                // ACL buffer: header + one ACCESS_ALLOWED_ACE.
                // ACE = ACE_HEADER(4) + ACCESS_MASK(4) + SID(sid_len).
                let acl_size = mem::size_of::<ACL>() as u32 + 4 + 4 + sid_len;
                let mut acl_buf = vec![0u8; acl_size as usize];
                let acl_ptr = acl_buf.as_mut_ptr() as *mut ACL;
                InitializeAcl(acl_ptr, acl_size, ACL_REVISION)?;

                let mask = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
                AddAccessAllowedAce(acl_ptr, ACL_REVISION, mask, psid)?;

                // Security descriptor referencing the ACL.
                let sd_size = mem::size_of::<SECURITY_DESCRIPTOR>();
                let mut sd_buf = vec![0u8; sd_size];
                let psd = PSECURITY_DESCRIPTOR(sd_buf.as_mut_ptr() as *mut c_void);
                // SECURITY_DESCRIPTOR_REVISION = 1
                InitializeSecurityDescriptor(psd, 1)?;
                SetSecurityDescriptorDacl(psd, true, Some(acl_ptr), false)?;

                let sa = SECURITY_ATTRIBUTES {
                    nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: psd.0,
                    bInheritHandle: false.into(),
                };

                Ok(Self {
                    _sd_buf: sd_buf,
                    _acl_buf: acl_buf,
                    sa,
                    owner_sid: sid_bytes,
                })
            }
        }

        /// Raw pointer suitable for
        /// `ServerOptions::create_with_security_attributes_raw`.
        pub fn security_attributes_ptr(&self) -> *mut c_void {
            &self.sa as *const SECURITY_ATTRIBUTES as *mut c_void
        }

        /// The pipe owner's raw SID bytes.
        pub fn owner_sid(&self) -> &[u8] {
            &self.owner_sid
        }

        /// Verify the process at the other end of `pipe_handle` runs under the
        /// same Windows user SID as the pipe owner (spec section 28.3).
        pub fn verify_client_sid(&self, pipe_handle: HANDLE) -> Result<bool, PipeSecurityError> {
            unsafe {
                let mut client_pid = 0u32;
                GetNamedPipeClientProcessId(pipe_handle, &mut client_pid)?;

                let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, client_pid)?;
                let mut tok = HANDLE::default();
                let r = OpenProcessToken(proc, TOKEN_QUERY, &mut tok);
                let _ = CloseHandle(proc);
                r?;

                let mut size = 0u32;
                let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut size);
                let mut buf = vec![0u8; size as usize];
                GetTokenInformation(
                    tok,
                    TokenUser,
                    Some(buf.as_mut_ptr() as *mut c_void),
                    size,
                    &mut size,
                )?;
                let _ = CloseHandle(tok);

                let tu = &*(buf.as_ptr() as *const TOKEN_USER);
                let client_psid = tu.User.Sid;
                let owner_psid = PSID(self.owner_sid.as_ptr() as *mut c_void);

                Ok(EqualSid(client_psid, owner_psid).is_ok())
            }
        }
    }
}

#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
pub fn get_current_user_sid() -> Result<(Vec<u8>, String), PipeSecurityError> {
    Err(PipeSecurityError::NotSupported)
}

#[cfg(not(windows))]
pub fn pipe_name_for_current_user() -> Result<String, PipeSecurityError> {
    Err(PipeSecurityError::NotSupported)
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_to_string_local_system() {
        // S-1-5-18 (Local System)
        let bytes = [
            1, 1, // Revision=1, SubCount=1
            0, 0, 0, 0, 0, 5, // Authority = 5 (NT Authority)
            18, 0, 0, 0, // Sub[0] = 18
        ];
        assert_eq!(sid_to_string(&bytes), "S-1-5-18");
    }

    #[test]
    fn sid_to_string_multiple_sub_authorities() {
        // S-1-5-21-100-200-300
        let bytes = [
            1, 4, // Revision=1, SubCount=4
            0, 0, 0, 0, 0, 5, // Authority = 5
            21, 0, 0, 0, // Sub[0] = 21
            100, 0, 0, 0, // Sub[1] = 100
            200, 0, 0, 0, // Sub[2] = 200
            44, 1, 0, 0, // Sub[3] = 300
        ];
        assert_eq!(sid_to_string(&bytes), "S-1-5-21-100-200-300");
    }

    #[cfg(windows)]
    #[test]
    fn get_current_user_sid_returns_valid_sid() {
        let (bytes, string) = get_current_user_sid().unwrap();
        assert!(!bytes.is_empty());
        assert!(string.starts_with("S-1-"));
        // Verify round-trip: manual conversion matches.
        assert_eq!(sid_to_string(&bytes), string);
    }

    #[cfg(windows)]
    #[test]
    fn pipe_security_can_be_created() {
        let sec = PipeSecurity::new().unwrap();
        assert!(!sec.owner_sid().is_empty());
        assert!(!sec.security_attributes_ptr().is_null());
    }

    #[cfg(windows)]
    #[test]
    fn pipe_name_contains_sid() {
        let name = pipe_name_for_current_user().unwrap();
        assert!(name.starts_with(r"\\.\pipe\wtd-S-1-"));
    }
}
