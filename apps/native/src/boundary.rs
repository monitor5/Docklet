//! Everything that crosses between the window and what drives it.
//!
//! The window knows nothing about Docker. It draws the plain data below and
//! reports what the user does as [`UiEvent`]s. Another thread hands it new
//! data through a [`UiHandle`]: [`UiHandle::send`] queues an [`UiUpdate`] and
//! posts one message to the UI thread, which swaps the data in and repaints.
//! Events are delivered on the UI thread to the closure given to
//! [`crate::run`]; it must return quickly, so it should only forward them.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Index of a tab, and of the table it shows.
pub type TabIndex = usize;

// ---------------------------------------------------------------------------
// Tables

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortBy {
    /// Text, with runs of digits compared as numbers.
    #[default]
    Text,
    /// The row's `keys` value for this column, for sizes and times whose text
    /// does not sort.
    Key,
    /// Clicking the header does nothing.
    Unsorted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub title: String,
    pub align: Align,
    /// Share of the width that is left once every column has its minimum.
    pub weight: u16,
    /// Narrowest the column gets, in device-independent pixels.
    pub min_width: u16,
    pub sort: SortBy,
    /// The cell shows the row's state: drawn in the "good" colour when the
    /// row has [`RowFlags::ACTIVE`] and muted otherwise.
    pub status: bool,
}

impl Column {
    pub fn new(title: impl Into<String>, weight: u16) -> Self {
        Self {
            title: title.into(),
            align: Align::Left,
            weight,
            min_width: 60,
            sort: SortBy::Text,
            status: false,
        }
    }

    pub fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }

    pub fn min_width(mut self, dip: u16) -> Self {
        self.min_width = dip;
        self
    }

    pub fn sort(mut self, sort: SortBy) -> Self {
        self.sort = sort;
        self
    }

    pub fn status(mut self) -> Self {
        self.status = true;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionKind {
    #[default]
    Plain,
    /// Removes the row: reported as [`UiEvent::RemoveRequested`], the same
    /// path as the selection bar and the Delete key.
    Remove,
}

/// A per-row action: a button in the last column and a context-menu item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// Reported back in [`UiEvent::RowAction`].
    pub key: String,
    pub label: String,
    pub kind: ActionKind,
}

impl Action {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: ActionKind::Plain,
        }
    }

    pub fn remove(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: ActionKind::Remove,
            ..Self::new(key, label)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowFlags(pub u32);

impl RowFlags {
    /// Running, healthy: status cells use the "good" colour.
    pub const ACTIVE: Self = Self(1);
    /// Drawn in the muted colour.
    pub const MUTED: Self = Self(2);
    /// Something is in progress: every action is disabled.
    pub const BUSY: Self = Self(4);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for RowFlags {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Stable across updates; selection and focus follow it.
    pub id: String,
    /// One per column.
    pub cells: Vec<String>,
    /// Sort keys for columns that sort by [`SortBy::Key`]; missing ones are 0.
    pub keys: Vec<i64>,
    /// Index into [`Table::groups`].
    pub group: Option<u32>,
    pub flags: RowFlags,
    /// Bit `i` set: action `i` of the table is available for this row.
    pub actions: u32,
}

impl Row {
    pub fn new(id: impl Into<String>, cells: Vec<String>) -> Self {
        Self {
            id: id.into(),
            cells,
            keys: Vec::new(),
            group: None,
            flags: RowFlags::default(),
            actions: u32::MAX,
        }
    }

    pub fn action_enabled(&self, action: usize) -> bool {
        action < 32 && self.actions & (1 << action) != 0 && !self.flags.contains(RowFlags::BUSY)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<Column>,
    /// Group titles. Grouped rows come first, group by group in this order,
    /// each under a header row; ungrouped rows follow.
    pub groups: Vec<String>,
    /// At most 32.
    pub actions: Vec<Action>,
    /// What Enter and a double click do.
    pub default_action: Option<usize>,
    pub rows: Vec<Row>,
    /// Plural noun for the count, such as "containers".
    pub noun: String,
    /// Shown when there are no rows at all.
    pub empty_text: String,
}

// ---------------------------------------------------------------------------
// Usage meters

#[derive(Clone, Debug, PartialEq)]
pub enum MeterValue {
    /// `fill` runs from 0 to 1.
    Known {
        text: String,
        fill: f32,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    pub label: String,
    pub value: MeterValue,
}

// ---------------------------------------------------------------------------
// Message screens, header and banner

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    /// Reported back in [`UiEvent::Button`].
    pub key: String,
    pub label: String,
}

impl Button {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Progress {
    #[default]
    None,
    /// Something is happening; how far along is unknown.
    Busy,
    /// From 0 to 1.
    Fraction(f32),
}

/// A full-window message: WSL missing, not installed, installing, stopped,
/// sleeping, starting, error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    pub title: String,
    pub body: String,
    /// Selectable text under the body: an error, a command, install output.
    pub detail: Option<String>,
    pub progress: Progress,
    pub primary: Option<Button>,
    pub secondary: Option<Button>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Message(Screen),
    /// The tabs and their tables.
    Tables,
}

impl Default for Page {
    fn default() -> Self {
        Self::Message(Screen::default())
    }
}

/// The right end of the top row: engine state and one button.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub status: String,
    pub button: Option<Button>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Info,
    Warning,
    Error,
}

/// A line above the content with a Dismiss button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub tone: Tone,
}

// ---------------------------------------------------------------------------
// Dialogs and the tray

/// A question with a Cancel button, answered by [`UiEvent::Confirmed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    /// Comes back in the answer, so it can name what was asked.
    pub token: u64,
    pub title: String,
    pub message: String,
    /// The label of the button that agrees, such as "Remove 7".
    pub accept: String,
}

/// A result to read and close, such as the outcome of a bulk removal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub message: String,
    /// Behind a "Show details" toggle: per-item failures, for instance.
    pub detail: Option<String>,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayState {
    pub tooltip: String,
    pub can_start: bool,
    pub can_stop: bool,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            tooltip: "Docklet".into(),
            can_start: false,
            can_stop: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Logs

/// Lines that the log view reads while it paints, at most a page at a time.
///
/// Lines have absolute numbers that keep counting when old lines are dropped,
/// so `bounds` is `(first, end)` with `end` one past the newest line.
/// Implementations lock briefly; the view copies what it needs and lets go
/// before it draws.
pub trait LogSource: Send + Sync {
    fn bounds(&self) -> (u64, u64);
    /// Changes whenever lines are added or dropped. The view repaints only
    /// when it has changed.
    fn generation(&self) -> u64;
    /// Calls `visit` with up to `count` lines from `first` on, in order, with
    /// each line's number.
    fn visit(&self, first: u64, count: usize, visit: &mut dyn FnMut(u64, &str));
}

/// A ring of the last `capacity` lines, ready to be a [`LogSource`].
pub struct LogRing {
    inner: Mutex<RingInner>,
    generation: AtomicU64,
}

struct RingInner {
    lines: VecDeque<String>,
    capacity: usize,
    first: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LogRing {
    /// The log view is built for 50,000 lines.
    pub const DEFAULT_CAPACITY: usize = 50_000;

    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(RingInner {
                lines: VecDeque::new(),
                capacity: capacity.max(1),
                first: 0,
            }),
            generation: AtomicU64::new(0),
        }
    }

    /// Adds `count` lines under one lock; `write` fills a cleared string per
    /// line. A full ring reuses the string of the line it drops, so a steady
    /// log allocates nothing.
    pub fn push_batch(&self, count: usize, mut write: impl FnMut(&mut String)) {
        if count == 0 {
            return;
        }
        let mut inner = lock(&self.inner);
        for _ in 0..count {
            let mut line = if inner.lines.len() >= inner.capacity {
                inner.first += 1;
                inner.lines.pop_front().unwrap_or_default()
            } else {
                String::new()
            };
            line.clear();
            write(&mut line);
            inner.lines.push_back(line);
        }
        drop(inner);
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn push(&self, text: &str) {
        self.push_batch(1, |line| line.push_str(text));
    }

    /// Drops every line and gives the memory back. Numbering continues.
    pub fn clear(&self) {
        let mut inner = lock(&self.inner);
        inner.first += inner.lines.len() as u64;
        inner.lines = VecDeque::new();
        drop(inner);
        self.generation.fetch_add(1, Ordering::Release);
    }
}

impl LogSource for LogRing {
    fn bounds(&self) -> (u64, u64) {
        let inner = lock(&self.inner);
        (inner.first, inner.first + inner.lines.len() as u64)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn visit(&self, first: u64, count: usize, visit: &mut dyn FnMut(u64, &str)) {
        let inner = lock(&self.inner);
        let start = first.max(inner.first);
        let Ok(offset) = usize::try_from(start - inner.first) else {
            return;
        };
        for (index, line) in inner.lines.iter().skip(offset).take(count).enumerate() {
            visit(start + index as u64, line);
        }
    }
}

/// A log shown in place of the table, until the user closes it.
#[derive(Clone)]
pub struct LogPane {
    pub title: String,
    pub source: Arc<dyn LogSource>,
}

impl fmt::Debug for LogPane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogPane")
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Updates in, events out

/// New data for the window. Each variant replaces what it names.
#[derive(Debug)]
pub enum UiUpdate {
    Page(Page),
    Header(Header),
    Banner(Option<Banner>),
    /// Tab titles; tab `i` shows table `i`.
    Tabs(Vec<String>),
    Table {
        tab: TabIndex,
        table: Table,
    },
    /// An empty list hides the strip.
    Usage(Vec<Meter>),
    /// Opens the log view over the table area.
    ShowLog(LogPane),
    /// A word after the line count, such as "log ended".
    LogNote(String),
    CloseLog,
    Confirm(Confirm),
    Notice(Notice),
    Tray(TrayState),
    ClearSelection {
        tab: TabIndex,
    },
    /// Brings the window up, creating it if needed.
    ShowWindow,
    /// Closes the window, removes the tray icon and ends [`crate::run`].
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    StartEngine,
    StopEngine,
    Quit,
}

/// Everything the user can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// The window was created or restored. Data may flow again.
    WindowShown,
    /// The window was closed (only the tray is left) or minimised. Nothing
    /// is drawn until [`UiEvent::WindowShown`]; stop polling.
    WindowHidden,
    TabChanged {
        tab: TabIndex,
    },
    FilterChanged {
        tab: TabIndex,
        text: String,
    },
    /// The table is already sorted; this is for remembering the choice.
    SortChanged {
        tab: TabIndex,
        column: usize,
        ascending: bool,
    },
    /// The checked rows, in display order.
    SelectionChanged {
        tab: TabIndex,
        ids: Vec<String>,
    },
    /// A row's button or context-menu item, or Enter on the row.
    RowAction {
        tab: TabIndex,
        id: String,
        action: String,
    },
    /// Remove these rows: from the selection bar, the Delete key or a row's
    /// remove action. Ask with [`UiUpdate::Confirm`] before doing it.
    RemoveRequested {
        tab: TabIndex,
        ids: Vec<String>,
    },
    /// The answer to a [`UiUpdate::Confirm`].
    Confirmed {
        token: u64,
        accepted: bool,
    },
    /// A [`Button`] of a message screen or of the header.
    Button {
        key: String,
    },
    BannerDismissed,
    Tray(TrayCommand),
    LogOpened,
    /// The user closed the log view, or it went with the window.
    LogClosed,
    /// F5.
    Refresh,
}

pub(crate) const WM_UI_UPDATE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

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
            }),
        }
    }

    /// Queues `update`. However many are sent, at most one message is in
    /// the UI thread's queue at a time.
    pub fn send(&self, update: UiUpdate) {
        lock(&self.shared.queue).push(update);
        if !self.shared.posted.swap(true, Ordering::SeqCst) && !self.post() {
            // Not running yet: `attach` drains what is queued.
            self.shared.posted.store(false, Ordering::SeqCst);
        }
    }

    fn post(&self) -> bool {
        let target = self.shared.target.load(Ordering::SeqCst);
        if target == 0 {
            return false;
        }
        // SAFETY: posting copies two integers into another thread's queue;
        // a stale handle fails harmlessly.
        unsafe {
            PostMessageW(
                Some(HWND(target as *mut _)),
                WM_UI_UPDATE,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .is_ok()
    }

    pub(crate) fn attach(&self, hwnd: HWND) {
        self.shared.target.store(hwnd.0 as isize, Ordering::SeqCst);
        self.shared.posted.store(true, Ordering::SeqCst);
        if !self.post() {
            self.shared.posted.store(false, Ordering::SeqCst);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(ring: &LogRing, first: u64, count: usize) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        ring.visit(first, count, &mut |number, line| {
            out.push((number, line.to_string()))
        });
        out
    }

    #[test]
    fn a_full_ring_drops_the_oldest_line_and_keeps_numbering() {
        let ring = LogRing::new(3);
        for text in ["a", "b", "c", "d", "e"] {
            ring.push(text);
        }
        assert_eq!(ring.bounds(), (2, 5));
        assert_eq!(
            lines(&ring, 0, 10),
            vec![(2, "c".into()), (3, "d".into()), (4, "e".into())]
        );
        assert_eq!(lines(&ring, 3, 1), vec![(3, "d".into())]);
        assert!(lines(&ring, 9, 1).is_empty());
    }

    #[test]
    fn every_change_moves_the_generation() {
        let ring = LogRing::new(3);
        let start = ring.generation();
        ring.push_batch(2, |line| line.push('x'));
        assert_ne!(ring.generation(), start);
        let before = ring.generation();
        ring.push_batch(0, |_| {});
        assert_eq!(ring.generation(), before);
        ring.clear();
        assert_ne!(ring.generation(), before);
    }

    #[test]
    fn clearing_keeps_the_absolute_numbers_going() {
        let ring = LogRing::new(3);
        ring.push("a");
        ring.push("b");
        ring.clear();
        assert_eq!(ring.bounds(), (2, 2));
        ring.push("c");
        assert_eq!(lines(&ring, 0, 5), vec![(2, "c".into())]);
    }

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
    fn row_actions_respect_the_mask_and_busy_rows() {
        let mut row = Row::new("x", vec![]);
        row.actions = 0b101;
        assert!(row.action_enabled(0));
        assert!(!row.action_enabled(1));
        assert!(row.action_enabled(2));
        assert!(!row.action_enabled(40));
        row.flags = RowFlags::BUSY;
        assert!(!row.action_enabled(0));
    }
}
