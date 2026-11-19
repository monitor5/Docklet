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
//!
//! An idle-stop proxy also answers report requests: setting the report
//! event makes it write what its connections are doing to a file and set
//! the done event (see `request_report`). It costs nothing until asked.

use std::io;
use std::time::{Duration, Instant};

use docklet_core::ProxyMode;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, ResetEvent,
    SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows::core::{PCWSTR, w};

const STOP_EVENT: PCWSTR = w!("Local\\docklet-proxy-stop");
const IDLE_STOP_EVENT: PCWSTR = w!("Local\\docklet-proxy-idle-stop");
const REPORT_EVENT: PCWSTR = w!("Local\\docklet-proxy-report");
const REPORT_DONE_EVENT: PCWSTR = w!("Local\\docklet-proxy-report-done");

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

/// Held by an idle-stop proxy that writes reports: a request event other
/// processes set, and a done event the proxy sets when the report is there.
/// Both reset themselves when a waiter wakes.
pub(crate) struct ReportSignal {
    request: HANDLE,
    done: HANDLE,
}

// The handles are only waited on and set; Win32 event handles are
// thread-safe.
unsafe impl Send for ReportSignal {}
unsafe impl Sync for ReportSignal {}

impl ReportSignal {
    pub(crate) fn create() -> io::Result<Self> {
        // SAFETY: plain Win32 calls; the handles are closed in `Drop`, or
        // here if the second call fails.
        unsafe {
            let request =
                CreateEventW(None, false, false, REPORT_EVENT).map_err(io::Error::other)?;
            match CreateEventW(None, false, false, REPORT_DONE_EVENT) {
                Ok(done) => Ok(Self { request, done }),
                Err(err) => {
                    let _ = CloseHandle(request);
                    Err(io::Error::other(err))
                }
            }
        }
    }

    /// Blocks the calling thread until a report is requested.
    pub(crate) fn wait(&self) {
        loop {
            // SAFETY: the handle is valid for the lifetime of `self`.
            if unsafe { WaitForSingleObject(self.request, INFINITE) } == WAIT_OBJECT_0 {
                return;
            }
            // A failed wait returns at once; never retry it in a tight loop.
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Tells the process that asked that the report is written.
    pub(crate) fn done(&self) {
        // SAFETY: the handle is valid for the lifetime of `self`.
        unsafe {
            let _ = SetEvent(self.done);
        }
    }
}

impl Drop for ReportSignal {
    fn drop(&mut self) {
        // SAFETY: both handles came from CreateEventW.
        unsafe {
            let _ = CloseHandle(self.request);
            let _ = CloseHandle(self.done);
        }
    }
}

/// Asks the running idle-stop proxy to write its report (to the file
/// `docklet_core::idle::report_path` names) and waits for it, for at most
/// `timeout`. False when no such proxy runs or it did not answer in time;
/// a proxy of an older version never answers.
pub fn request_report(timeout: Duration) -> bool {
    let Some(request) = open(REPORT_EVENT) else {
        return false;
    };
    // SAFETY: plain Win32 call with a static name.
    let done = unsafe {
        OpenEventW(
            SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE,
            false,
            REPORT_DONE_EVENT,
        )
    };
    let answered = match done {
        Ok(done) => {
            let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            // SAFETY: both handles are open with the rights these calls need
            // and are closed below.
            let answered = unsafe {
                // A done left over from an earlier request that nobody
                // waited for must not count for this one.
                let _ = ResetEvent(done);
                let _ = SetEvent(request);
                WaitForSingleObject(done, millis) == WAIT_OBJECT_0
            };
            // SAFETY: the handle came from OpenEventW.
            unsafe {
                let _ = CloseHandle(done);
            }
            answered
        }
        Err(_) => false,
    };
    // SAFETY: the handle came from OpenEventW.
    unsafe {
        let _ = CloseHandle(request);
    }
    answered
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
