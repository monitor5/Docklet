//! The widgets, each driven by plain data from [`crate::boundary`].
//!
//! The `*_state`, `columns` and `ansi` modules are pure logic with tests; the
//! others own Win32 controls and leave input handling to the shell.

pub mod ansi;
pub mod banner;
pub mod columns;
pub mod dialog;
pub mod filter;
pub mod log_state;
pub mod log_view;
pub mod message;
pub mod selection_bar;
pub mod table;
pub mod table_state;
pub mod tabs;
pub mod usage;
