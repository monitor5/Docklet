//! Security descriptor that restricts a named pipe to the current user.

use std::ffi::c_void;
use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// Owns a security descriptor that grants full access to the current user and
/// to SYSTEM, and nothing to anyone else (protected DACL, no inheritance).
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// The descriptor is an immutable heap block owned by this struct.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    pub fn current_user_only() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid NUL-terminated string and `descriptor`
        // receives a LocalAlloc'ed block that `Drop` frees.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(io::Error::other)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    /// Pointer to a `SECURITY_ATTRIBUTES` valid for as long as `self` lives.
    pub fn as_attributes_ptr(&self) -> *mut c_void {
        (&raw const self.attributes).cast_mut().cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

/// Returns the SID of the user that owns this process, e.g. `S-1-5-21-...`.
fn current_user_sid() -> io::Result<String> {
    // SAFETY: plain Win32 calls with buffers sized by the API itself; every
    // handle and allocation is released before returning.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io::Error::other)?;

        let mut needed = 0u32;
        // First call reports the required size and fails by design.
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        // u64 storage keeps the buffer aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        );
        let _ = CloseHandle(token);
        result.map_err(io::Error::other)?;

        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid_text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_text).map_err(io::Error::other)?;
        let sid = sid_text.to_string().map_err(io::Error::other);
        LocalFree(Some(HLOCAL(sid_text.0.cast())));
        sid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_sid_is_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "unexpected SID: {sid}");
    }

    #[test]
    fn builds_descriptor() {
        let security = PipeSecurity::current_user_only().unwrap();
        assert!(!security.as_attributes_ptr().is_null());
    }
}
