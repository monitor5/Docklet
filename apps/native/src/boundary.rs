//! Everything that crosses between the window and what drives it.
//!
//! The window knows nothing about Docker and keeps no state of the lists.
//! It draws the plain data of `docklet_app::view`, re-exported here, and
//! reports what the user does as [`UiEvent`]s. Events are delivered on the
//! UI thread to the closure given to [`crate::run`]; it must return quickly.
//!
//! New data comes through a [`UiHandle`], from any thread:
//! [`UiHandle::send`] queues an [`UiUpdate`] and posts one message to the UI
//! thread, which swaps the data in and repaints. A thread that has something
//! for the UI thread other than an update calls [`UiHandle::wake`], and the
//! closure is called with [`UiEvent::Wake`].

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use docklet_app::logs::LogSource;
pub use docklet_app::view::*;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub(crate) const WM_UI_UPDATE: u32 = WM_APP + 10;
pub(crate) const WM_UI_WAKE: u32 = WM_APP + 13;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hands updates to the UI thread from any thread. Cheap to clone.
#[derive(Clone)]
pub struct UiHandle {
    shared: Arc<Shared>,
}

struct Shared {
    queue: Mutex<Vec<UiUpdate>>,
    /// The window that drains the queue; 0 before [`crate::run`] starts and
    /// after it ends.
    target: AtomicIsize,
    /// A drain message is on its way, so another one is not needed.
    posted: AtomicBool,
    /// A wake-up was asked for and has not been delivered.
    woken: AtomicBool,
    /// A wake-up was asked for before there was a window to post it to.
    pending_wake: AtomicBool,
}

impl Default for UiHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UiHandle {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(Vec::new()),
                target: AtomicIsize::new(0),
                posted: AtomicBool::new(false),
                woken: AtomicBool::new(false),
                pending_wake: AtomicBool::new(false),
            }),
        }
    }

    /// Queues `update`. However many are sent, at most one message is in
    /// the UI thread's queue at a time.
    pub fn send(&self, update: UiUpdate) {
        lock(&self.shared.queue).push(update);
        if !self.shared.posted.swap(true, Ordering::SeqCst) && !self.post(WM_UI_UPDATE) {
            // Not running yet: `attach` drains what is queued.
            self.shared.posted.store(false, Ordering::SeqCst);
        }
    }

    /// Asks for [`UiEvent::Wake`] on the UI thread. However often it is
    /// called before that arrives, it arrives once.
    pub fn wake(&self) {
        if !self.shared.woken.swap(true, Ordering::SeqCst) && !self.post(WM_UI_WAKE) {
            // Not running yet: `attach` delivers it.
            self.shared.woken.store(false, Ordering::SeqCst);
            self.shared.pending_wake.store(true, Ordering::SeqCst);
        }
    }

    fn post(&self, message: u32) -> bool {
        let target = self.shared.target.load(Ordering::SeqCst);
        if target == 0 {
            return false;
        }
        // SAFETY: posting copies two integers into another thread's queue;
        // a stale handle fails harmlessly.
        unsafe { PostMessageW(Some(HWND(target as *mut _)), message, WPARAM(0), LPARAM(0)) }.is_ok()
    }

    pub(crate) fn attach(&self, hwnd: HWND) {
        self.shared.target.store(hwnd.0 as isize, Ordering::SeqCst);
        self.shared.posted.store(true, Ordering::SeqCst);
        if !self.post(WM_UI_UPDATE) {
            self.shared.posted.store(false, Ordering::SeqCst);
        }
        if self.shared.pending_wake.swap(false, Ordering::SeqCst) {
            self.wake();
        }
    }

    pub(crate) fn detach(&self) {
        self.shared.target.store(0, Ordering::SeqCst);
        lock(&self.shared.queue).clear();
    }

    /// Everything queued so far, oldest first.
    pub(crate) fn take(&self) -> Vec<UiUpdate> {
        // Cleared before taking: a send in between posts again, which costs
        // one empty drain at worst and never loses an update.
        self.shared.posted.store(false, Ordering::SeqCst);
        std::mem::take(&mut *lock(&self.shared.queue))
    }

    /// The wake-up is being delivered: the next call to `wake` posts again.
    /// Cleared before the event, so a wake asked for while it is handled is
    /// not lost.
    pub(crate) fn woke(&self) {
        self.shared.woken.store(false, Ordering::SeqCst);
    }
}

impl docklet_app::Ui for UiHandle {
    fn send(&self, update: UiUpdate) {
        UiHandle::send(self, update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_queue_until_the_window_exists() {
        let handle = UiHandle::new();
        handle.send(UiUpdate::ShowWindow);
        handle.send(UiUpdate::CloseLog);
        let taken = handle.take();
        assert_eq!(taken.len(), 2);
        assert!(matches!(taken[0], UiUpdate::ShowWindow));
        assert!(handle.take().is_empty());
    }

    #[test]
    fn a_wake_before_the_window_exists_waits_for_it() {
        let handle = UiHandle::new();
        handle.wake();
        handle.wake();
        // Nothing to post to yet: it is kept for `attach`.
        assert!(handle.shared.pending_wake.load(Ordering::SeqCst));
        assert!(!handle.shared.woken.load(Ordering::SeqCst));
    }
}
