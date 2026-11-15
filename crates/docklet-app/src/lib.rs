//! Docklet's application layer: everything a window does except drawing.
//!
//! It knows neither a UI toolkit nor Windows. A shell draws what the models
//! here hold and forwards input to them; Windows-only calls stay behind
//! `docklet-wsl`, `docklet-proxy` and `docklet-engine`, so this crate builds
//! and tests on Linux.
//!
//! # Two halves
//!
//! **Models**, which do no I/O and belong to the UI thread:
//!
//! - [`screen`]: which screen shows for an engine state, and its text.
//! - [`table`]: containers, images, volumes and networks as columns and
//!   rows of text; [`list::ListModel`] adds filter, sort order, collapsed
//!   groups and the selection ([`selection`]).
//! - [`removal`]: the question before a bulk removal or a prune, and the
//!   sentence after it.
//! - [`usage`]: the strip of meters, with every "not known" as a state.
//! - [`logs`]: the bounded log buffer; [`pull`]: the pull box.
//! - [`policy`]: when the engine is polled and sampled, and when it must
//!   not be touched.
//! - [`view`]: the plain data a window draws and the events it reports;
//!   [`present`] puts the models into that form.
//! - [`controller`]: the models, the window and the engine together. A
//!   window that sends its events there and draws what comes back is the
//!   whole application; it keeps no state of its own about the lists.
//!
//! **Services**, which do the I/O off the UI thread:
//!
//! - [`control`]: installing, starting and stopping the engine, one
//!   transition at a time. Blocking calls.
//! - [`streams`]: Docker's event stream with its reconnect backoff,
//!   followed logs, container stats and host usage, delivered to a sink.
//! - [`backend::Backend`]: the two together, for a shell that has its own
//!   way of running blocking work (the Tauri app).
//! - [`app::App`]: the whole application behind one handle, for a shell
//!   with a message loop: methods that return at once, results as
//!   [`app::Message`]s posted through a [`app::Sink`].
//!
//! # Threads, with `App`
//!
//! - The UI thread owns the models and calls the `App`'s methods. None of
//!   them blocks.
//! - The app's worker threads do the work and call `Sink::post`. The sink
//!   hands the message to the UI thread, which feeds it to the models.
//! - Nothing here calls back into the shell in any other way.
//!
//! # What never happens
//!
//! Nothing here wakes a sleeping engine or boots a stopped distro on its
//! own: Docker is only asked while the engine is known to run, streams are
//! only reopened when the engine can be reached without starting anything,
//! and the host's figures are read from Windows alone. Only `start` wakes.

pub mod app;
pub mod backend;
pub mod control;
pub mod controller;
pub mod format;
pub mod list;
pub mod logs;
pub mod policy;
pub mod present;
pub mod pull;
pub mod removal;
pub mod screen;
pub mod selection;
pub mod streams;
pub mod table;
pub mod usage;
pub mod view;

pub use app::{App, List, Message, Sink};
pub use backend::Backend;
pub use control::{
    EngineControl, EngineHost, EngineStatus, Supervisor, SystemHost, Transition, reachable,
};
pub use controller::{Controller, Engine, Ui};
pub use list::{Item, ListModel};
pub use logs::{LOG_BUFFER_CAP, LogBuffer, LogSource, SharedLog, strip_ansi};
pub use policy::WindowState;
pub use pull::PullProgress;
pub use removal::{PruneTarget, RemovalPlan, prune_summary, removal_summary};
pub use selection::{Coverage, Selection};
pub use streams::{DockerEvent, HostReading, LogMessage, StreamSink, Streams};
pub use table::{Align, Column, Filter, Row, RowAction, RowFlags, Sort, SortDirection, Table};
pub use usage::{Meter, MeterKind, MeterState, UsageModel};

// What a shell needs of the crates below, so it depends on this one alone.
pub use docklet_core::EngineState;
pub use docklet_engine::{
    ContainerAction, ContainerUsage, DiskUsage, PruneReport, RemoveReport, ResourceKind,
};
