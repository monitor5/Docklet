//! The window and the application joined: a `docklet_app::Controller` on the
//! UI thread, fed by the window's events and by the engine's messages.
//!
//! The engine posts its messages on worker threads. They wait in a channel
//! and the UI thread is woken ([`UiHandle::wake`]); it takes them out, hands
//! them to the controller and sends the window what changed, once for the
//! whole batch.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use docklet_app::list::Item;
use docklet_app::present::key;
use docklet_app::{Controller, Engine, Message, ResourceKind, Sink};

use crate::boundary::{UiEvent, UiHandle};
use crate::host::{RunOptions, run};

/// Most messages taken in before the window is told what changed and the
/// message loop gets its turn again; the rest follows with the next wake.
const BATCH: usize = 256;

/// Where an engine posts, and where its messages wait for the UI thread.
pub fn channel(handle: &UiHandle) -> (Arc<dyn Sink>, Receiver<Message>) {
    let (sender, receiver) = mpsc::channel();
    let handle = handle.clone();
    let sink = move |message: Message| {
        // The receiver goes when the UI ends; nobody is left to tell.
        if sender.send(message).is_ok() {
            handle.wake();
        }
    };
    (Arc::new(sink), receiver)
}

/// How [`run_engine`] starts.
#[derive(Clone, Debug, Default)]
pub struct Start {
    pub window: RunOptions,
    /// A line shown above the content from the start.
    pub notice: Option<String>,
    /// Open the log of the first container as soon as the list is there,
    /// for measuring the log view.
    pub open_first_log: bool,
}

/// Runs the UI over `engine` on this thread until the user quits.
/// `messages` is where the engine's sink puts what it posts (see
/// [`channel`]).
pub fn run_engine<E: Engine + 'static>(
    engine: E,
    handle: UiHandle,
    messages: Receiver<Message>,
    start: Start,
) {
    let mut controller = Controller::new(engine, handle.clone());
    if let Some(notice) = start.notice {
        controller.show_info(notice);
    }
    let mut open_first_log = start.open_first_log;
    let waker = handle.clone();
    run(handle, start.window, move |event| {
        if event != UiEvent::Wake {
            return controller.on_event(event);
        }
        // Not until the channel is empty: a sender that never pauses would
        // keep the message loop from its other work.
        let mut taken = 0;
        while taken < BATCH {
            let Ok(message) = messages.try_recv() else {
                break;
            };
            controller.handle_message(message);
            taken += 1;
        }
        if taken == BATCH {
            waker.wake();
        }
        controller.sync();
        if open_first_log && let Some(id) = first_row(&controller) {
            open_first_log = false;
            controller.on_event(UiEvent::RowAction {
                tab: 0,
                id,
                action: key::LOGS.to_string(),
            });
        }
    });
}

/// The container in the first row of the list, once there is one.
fn first_row<E: Engine>(controller: &Controller<E, UiHandle>) -> Option<String> {
    let list = controller.list(ResourceKind::Container);
    (0..list.len()).find_map(|index| match list.item(index)? {
        Item::Row(row) => Some(row.id.clone()),
        Item::Group { .. } => None,
    })
}
