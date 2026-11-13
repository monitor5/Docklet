//! Start/stop control of the background proxy process.
//!
//! The proxy owns a named event in the user's session. Its existence means a
//! proxy is running; setting it asks the proxy to exit. That avoids PID files
//! and the risk of terminating an unrelated process that reused a PID.

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::core::w;

const STOP_EVENT: windows::core::PCWSTR = w!("Local\\docklet-proxy-stop");

/// Held by the running proxy.
pub struct StopSignal(HANDLE);

// The handle is only waited on; Win32 event handles are thread-safe.
unsafe impl Send for StopSignal {}

impl StopSignal {
    /// Creates the stop event. Call once, in the proxy process.
    pub fn create() -> io::Result<Self> {
        // SAFETY: plain Win32 call; the handle is closed in `Drop`.
        let handle =
            unsafe { CreateEventW(None, true, false, STOP_EVENT) }.map_err(io::Error::other)?;
        Ok(Self(handle))
    }

    /// Blocks the calling thread until `request_stop` is called.
    pub fn wait(&self) {
        loop {
            // SAFETY: the handle is valid for the lifetime of `self`.
            if unsafe { WaitForSingleObject(self.0, INFINITE) } == WAIT_OBJECT_0 {
                return;
            }
            // A failed wait returns at once; never retry it in a tight loop.
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// True when a proxy process of this user is running.
pub fn is_running() -> bool {
    open().is_some_and(|handle| {
        // SAFETY: the handle came from OpenEventW.
        unsafe {
            let _ = CloseHandle(handle);
        }
        true
    })
}

/// Asks the running proxy to exit. Returns whether one was running.
pub fn request_stop() -> bool {
    let Some(handle) = open() else {
        return false;
    };
    // SAFETY: the handle came from OpenEventW with EVENT_MODIFY_STATE.
    unsafe {
        let _ = SetEvent(handle);
        let _ = CloseHandle(handle);
    }
    true
}

fn open() -> Option<HANDLE> {
    // SAFETY: plain Win32 call with a static name.
    unsafe { OpenEventW(EVENT_MODIFY_STATE, false, STOP_EVENT) }.ok()
}
