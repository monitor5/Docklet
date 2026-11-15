//! The widgets, each driven by plain data from [`crate::boundary`].
//!
//! The `log_state`, `lines`, `columns` and `ansi` modules are pure logic
//! with tests; the others own Win32 controls and leave input handling to the
//! shell.

pub mod ansi;
pub mod banner;
pub mod columns;
pub mod dialog;
pub mod entry;
pub mod filter;
pub mod lines;
pub mod log_state;
pub mod log_view;
pub mod message;
pub mod selection_bar;
pub mod table;
pub mod tabs;
pub mod usage;
