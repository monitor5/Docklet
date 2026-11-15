//! Start/stop control of the background proxy process.
//!
//! The proxy owns a named event in the user's session. Its existence means a
//! proxy is running; setting it asks the proxy to exit. That avoids PID files
//! and the risk of terminating an unrelated process that reused a PID.
//!
//! A proxy that serves with idle stop owns a second event, which only says
//! so by existing. Whoever shows the engine's state needs to know: behind
//! such a proxy a stopped distro is asleep, not stopped. Asking costs two
//! `OpenEventW` calls and no `wsl.exe`.

use std::io;
use std::time::{Duration, Instant};

use docklet_core::ProxyMode;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::core::{PCWSTR, w};

const STOP_EVENT: PCWSTR = w!("Local\\docklet-proxy-stop");
const IDLE_STOP_EVENT: PCWSTR = w!("Local\\docklet-proxy-idle-stop");

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
            std::thread::sleep(Duration::from_secs(1));
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

/// Held by a running proxy that serves with idle stop, for as long as it
/// runs. Nobody waits on it or sets it.
pub struct IdleStopMarker(HANDLE);

// The handle is only held; Win32 event handles are thread-safe.
unsafe impl Send for IdleStopMarker {}

impl IdleStopMarker {
    /// Creates the marker. Call in the proxy process before
    /// `StopSignal::create`, so that a proxy which counts as running is
    /// never taken for a plain one.
    pub fn create() -> io::Result<Self> {
        // SAFETY: plain Win32 call; the handle is closed in `Drop`.
        let handle = unsafe { CreateEventW(None, true, false, IDLE_STOP_EVENT) }
            .map_err(io::Error::other)?;
        Ok(Self(handle))
    }
}

impl Drop for IdleStopMarker {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// True when a proxy process of this user is running.
pub fn is_running() -> bool {
    exists(STOP_EVENT)
}

/// Which kind of proxy this user runs, if any. Cheap: no process is started.
pub fn mode() -> ProxyMode {
    if !is_running() {
        ProxyMode::Down
    } else if exists(IDLE_STOP_EVENT) {
        ProxyMode::IdleStop
    } else {
        ProxyMode::Plain
    }
}

/// Asks the running proxy to exit. Returns whether one was running.
pub fn request_stop() -> bool {
    let Some(handle) = open(STOP_EVENT) else {
        return false;
    };
    // SAFETY: the handle came from OpenEventW with EVENT_MODIFY_STATE.
    unsafe {
        let _ = SetEvent(handle);
        let _ = CloseHandle(handle);
    }
    true
}

/// Asks the running proxy to exit and waits until it is gone, for at most
/// five seconds. Returns whether no proxy is left, which is also the case
/// when none was running.
///
/// Waiting matters to whoever starts a proxy next: the stop event stays
/// signalled while the old proxy still holds it, so until then `is_running`
/// is true and a new proxy would exit at once.
pub fn stop() -> bool {
    if !request_stop() {
        return true;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while is_running() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn exists(event: PCWSTR) -> bool {
    open(event).is_some_and(|handle| {
        // SAFETY: the handle came from OpenEventW.
        unsafe {
            let _ = CloseHandle(handle);
        }
        true
    })
}

fn open(event: PCWSTR) -> Option<HANDLE> {
    // SAFETY: plain Win32 call with a static name.
    unsafe { OpenEventW(EVENT_MODIFY_STATE, false, event) }.ok()
}
