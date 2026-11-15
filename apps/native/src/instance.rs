//! One Docklet window per user session: a second start shows the first
//! one's window and exits.

use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
};
use windows::core::{PCWSTR, w};

use crate::host::{HOST_CLASS, WM_SHOW_REQUEST};

const MUTEX: PCWSTR = w!("Local\\Docklet.Native.SingleInstance");

/// Held for the life of the process; the mutex goes with it.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Claims the session's single instance. `None` means another instance
/// runs; it has been asked to show its window.
pub fn claim() -> Option<InstanceGuard> {
    // SAFETY: a named mutex with default security; the error is read at
    // once, before any other call can change it.
    let (handle, existed) = unsafe {
        let handle = CreateMutexW(None, false, MUTEX);
        let existed = GetLastError() == ERROR_ALREADY_EXISTS;
        (handle, existed)
    };
    match handle {
        Ok(handle) if !existed => Some(InstanceGuard(handle)),
        Ok(handle) => {
            drop(InstanceGuard(handle));
            signal_existing();
            None
        }
        // Without a mutex there is no way to tell; run rather than refuse.
        Err(_) => Some(InstanceGuard(HANDLE::default())),
    }
}

/// Asks the running instance to show its window. It may be starting up and
/// not have its window yet, so this looks a few times, a tenth of a second
/// apart, and gives up after two seconds.
fn signal_existing() {
    for _ in 0..20 {
        // SAFETY: the class name is static; the window belongs to another
        // process, so only posting and plain queries are used.
        unsafe {
            if let Ok(host) = FindWindowW(HOST_CLASS, PCWSTR::null()) {
                let mut process = 0u32;
                GetWindowThreadProcessId(host, Some(&mut process));
                // Lets the running instance bring its window to the front.
                let _ = AllowSetForegroundWindow(process);
                let _ = PostMessageW(Some(host), WM_SHOW_REQUEST, WPARAM(0), LPARAM(0));
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
