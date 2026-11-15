//! Everything that crosses between a window and what drives it.
//!
//! A window knows nothing about Docker and keeps no state of the lists: it
//! draws the plain data below and reports what the user does as
//! [`UiEvent`]s. Which rows show, in which order, and which of them are
//! selected is decided by the models (`list::ListModel`), put into this form
//! by `present`, and sent as a whole with every change.

use std::fmt;
use std::sync::Arc;

use crate::logs::LogSource;
pub use crate::selection::Coverage;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub title: String,
    pub align: Align,
    /// Share of the width that is left once every column has its minimum.
    pub weight: u16,
    /// Narrowest the column gets, in device-independent pixels.
    pub min_width: u16,
    /// A click on the header asks for another order
    /// ([`UiEvent::SortRequested`]).
    pub sortable: bool,
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
            sortable: true,
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

    pub fn status(mut self) -> Self {
        self.status = true;
        self
    }
}

/// Something that can be asked for by name: a row's action, a command of a
/// table's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// Reported back in [`UiEvent::RowAction`] or [`UiEvent::Command`].
    pub key: String,
    pub label: String,
    /// A row action that has a button in the last column. The others are
    /// only in the row's context menu.
    pub button: bool,
}

impl Action {
    /// An action with a button.
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            button: true,
        }
    }

    /// An action of the context menu only.
    pub fn in_menu(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            button: false,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// Names the row in every event about it.
    pub id: String,
    /// One per column.
    pub cells: Vec<String>,
    /// Whether the row's box is ticked; `None` for a row that has no box
    /// because it cannot be removed.
    pub check: Option<bool>,
    /// The row belongs to the group whose header is above it.
    pub grouped: bool,
    pub flags: RowFlags,
    /// Bit `i` set: action `i` of the table is available for this row.
    pub actions: u32,
    /// Actions only this row has, such as opening one of its ports. They
    /// follow the table's in the context menu.
    pub extra: Vec<Action>,
}

impl Row {
    pub fn new(id: impl Into<String>, cells: Vec<String>) -> Self {
        Self {
            id: id.into(),
            cells,
            check: Some(false),
            actions: u32::MAX,
            ..Self::default()
        }
    }

    pub fn action_enabled(&self, action: usize) -> bool {
        action < 32 && self.actions & (1 << action) != 0 && !self.flags.contains(RowFlags::BUSY)
    }
}

/// One line of a table, from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// The header of a group; its rows follow unless it is collapsed.
    Group {
        name: String,
        /// Rows of the group that match the filter, shown or not.
        count: usize,
        collapsed: bool,
        /// Whether the group's box is ticked: all the rows under it are
        /// selected. `None` when it has no box, as a collapsed group.
        check: Option<bool>,
    },
    Row(Row),
}

/// The arrow in a column header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortMark {
    pub column: usize,
    pub ascending: bool,
}

/// A list as it shows right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<Column>,
    /// What can be done to a row; at most 32.
    pub actions: Vec<Action>,
    /// What Enter and a double click on a row do.
    pub default_action: Option<usize>,
    pub lines: Vec<Line>,
    pub sort: Option<SortMark>,
    /// The "select all" box in the header.
    pub select_all: Coverage,
    /// How many rows are selected. The Remove and Clear buttons show while
    /// it is not zero.
    pub selected: usize,
    /// The line next to the filter: `12 of 200 containers`, `7 selected`,
    /// or why the list is empty.
    pub summary: String,
    /// The table's menu, such as its prunes ([`UiEvent::Command`]).
    pub commands: Vec<Action>,
}

/// A line of text to type above a table, with its button: the image to pull.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// Shown in the box while it is empty.
    pub cue: String,
    /// What the button says, such as `Pull` or `Pulling…`.
    pub button: String,
    /// False while what was typed is being worked on: the box and the
    /// button are disabled.
    pub enabled: bool,
    /// A line next to the button: the progress of what was submitted.
    pub status: String,
    /// When set, replaces what is typed (emptied after a pull that worked).
    pub text: Option<String>,
}

// ---------------------------------------------------------------------------
// Usage meters

/// One meter of the usage strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    pub label: String,
    /// The figure, or a word for why there is none: `Measuring…`, `Off`,
    /// `Unknown`, `Not measured`.
    pub text: String,
    /// How full to draw the bar, from 0 to 1; `None` draws an empty one.
    pub fill: Option<f32>,
    /// `text` is a figure.
    pub known: bool,
    /// A sentence about the figure, or about why there is none.
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Message screens, header and banner

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    /// Reported back in [`UiEvent::Button`].
    pub key: String,
    pub label: String,
    pub enabled: bool,
}

impl Button {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            enabled: true,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
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

/// One of a few options to pick on a message screen: the base of an install.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub options: Vec<String>,
    /// Index into `options`; a change is reported as
    /// [`UiEvent::ChoiceChanged`].
    pub selected: usize,
    pub enabled: bool,
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
    pub choice: Option<Choice>,
    pub primary: Option<Button>,
    pub secondary: Option<Button>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Message(Box<Screen>),
    /// The tabs and their tables.
    Tables,
}

impl Page {
    pub fn message(screen: Screen) -> Self {
        Self::Message(Box::new(screen))
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::message(Screen::default())
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
    /// What the filter box of a tab holds. Sent when the tab comes up, not
    /// while the user types: the box is theirs then.
    Filter {
        tab: TabIndex,
        text: String,
    },
    /// The line to type above a tab's table, or `None` for none.
    Entry {
        tab: TabIndex,
        entry: Option<Entry>,
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
    /// Opens `format::port_url` of this port in the browser.
    OpenPort(u16),
    /// Puts text on the clipboard.
    Copy(String),
    /// Brings the window up, creating it if needed.
    ShowWindow,
    /// Closes the window, removes the tray icon and ends the UI.
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
    /// is drawn until [`UiEvent::WindowShown`]: hold nothing for it.
    WindowHidden,
    /// Another thread asked for the UI thread's attention; nothing the
    /// user did.
    Wake,
    TabChanged {
        tab: TabIndex,
    },
    FilterChanged {
        tab: TabIndex,
        text: String,
    },
    /// A click on a column header.
    SortRequested {
        tab: TabIndex,
        column: usize,
    },
    /// A row's box, by click or Space. `range` with Shift held: from the
    /// row last toggled to this one.
    RowToggled {
        tab: TabIndex,
        id: String,
        range: bool,
    },
    /// A group header's box.
    GroupToggled {
        tab: TabIndex,
        name: String,
    },
    /// A group was collapsed or expanded.
    GroupFolded {
        tab: TabIndex,
        name: String,
    },
    /// The "select all" box in the header, or Ctrl+A.
    AllToggled {
        tab: TabIndex,
    },
    /// The Clear button, or Escape.
    SelectionCleared {
        tab: TabIndex,
    },
    /// A row's button or context-menu item, or Enter on the row.
    RowAction {
        tab: TabIndex,
        id: String,
        action: String,
    },
    /// Remove the selected rows (the selection bar, the Delete key), or the
    /// one row named when nothing is selected. Ask with
    /// [`UiUpdate::Confirm`] before doing it.
    RemoveRequested {
        tab: TabIndex,
        id: Option<String>,
    },
    /// An item of the table's menu.
    Command {
        tab: TabIndex,
        key: String,
    },
    /// The button of a tab's [`Entry`], or Enter in its box.
    Submitted {
        tab: TabIndex,
        text: String,
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
    /// Another option of a message screen's [`Choice`] was picked.
    ChoiceChanged {
        index: usize,
    },
    BannerDismissed,
    /// A click on the usage strip: say what the meters mean.
    UsageClicked,
    Tray(TrayCommand),
    LogOpened,
    /// The user closed the log view, or it went with the window.
    LogClosed,
    /// F5.
    Refresh,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn actions_are_buttons_unless_they_are_for_the_menu() {
        assert!(Action::new("logs", "Logs").button);
        assert!(!Action::in_menu("restart", "Restart").button);
    }
}
