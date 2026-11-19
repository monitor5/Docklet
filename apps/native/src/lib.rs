//! Docklet's native Win32 shell: the window, its widgets and the tray icon.
//!
//! The shell knows nothing about Docker and keeps no state of the lists. It
//! draws the plain data of [`boundary`] and reports what the user does as
//! [`UiEvent`]s; see that module for how another thread feeds it. [`run`]
//! owns the UI thread.
//!
//! Layout of the crate:
//! - [`boundary`]: the data types and events of `docklet_app::view`, and
//!   [`UiHandle`];
//! - `host`: the process-long hidden window, tray, model and message loop;
//! - `shell`: the main window, created on demand and destroyed on close;
//! - `widgets`: one module per widget, plus their pure logic;
//! - [`app`]: the window joined with a `docklet_app::Controller`, which is
//!   Docklet;
//! - [`demo`]: a made-up engine for `--demo`, behind the same controller;
//! - [`proxy`]: `--proxy`, the pipe proxy this executable also is.

#![cfg(windows)]

pub mod app;
pub mod boundary;
pub mod demo;
mod host;
pub mod instance;
pub mod proxy;
mod shell;
mod theme;
mod tray;
mod widgets;
mod win;

pub use boundary::*;
pub use host::{RunOptions, run};
