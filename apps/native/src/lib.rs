//! Docklet's native Win32 shell: the window, its widgets and the tray icon.
//!
//! The shell knows nothing about Docker. It draws the plain data of
//! [`boundary`] and reports what the user does as [`UiEvent`]s; see that
//! module for how another thread feeds it. [`run`] owns the UI thread.
//!
//! Layout of the crate:
//! - [`boundary`]: data types, events, [`UiHandle`], [`LogRing`];
//! - `host`: the process-long hidden window, tray, model and message loop;
//! - `shell`: the main window, created on demand and destroyed on close;
//! - `widgets`: one module per widget, plus their pure logic;
//! - [`demo`]: fake data for `--demo`.

#![cfg(windows)]

pub mod boundary;
pub mod demo;
mod host;
pub mod instance;
mod shell;
mod theme;
mod tray;
mod widgets;
mod win;

pub use boundary::*;
pub use host::{RunOptions, run};
