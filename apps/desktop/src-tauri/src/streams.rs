//! Long-lived Docker streams, adapted from `docklet-engine` callbacks to the
//! webview: events become Tauri events, log lines go through a channel.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use docklet_engine::{Backoff, Client};
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

use crate::supervisor;

/// Event sent to the UI whenever Docker reports a change, when the event
/// stream ends and when it is about to be opened again.
const DOCKER_EVENT: &str = "docker-event";

/// An event stream that lasted this long was working, so the next reconnect
/// starts with the shortest wait again.
const STABLE_STREAM: Duration = Duration::from_secs(30);

/// What a followed log sends to the webview.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogMessage {
    Lines {
        lines: Vec<String>,
    },
    /// The last message of a log. `error` is null when the log simply ended,
    /// which it does when its container stops or is removed.
    Ended {
        error: Option<String>,
    },
}

/// The background tasks that feed the UI.
#[derive(Default)]
pub struct Streams {
    events: Mutex<Option<JoinHandle<()>>>,
    logs: Mutex<HashMap<u32, JoinHandle<()>>>,
    next_log: AtomicU32,
}

impl Streams {
    /// Subscribes to Docker's event stream and forwards each event to the UI,
    /// which refetches the affected list. The subscription is renewed when it
    /// ends, until `stop_all`. Idempotent.
    pub fn watch_events(&self, app: &AppHandle) {
        let mut slot = self.events.lock().expect("events lock");
        if slot
            .as_ref()
            .is_some_and(|task| !task.inner().is_finished())
        {
            return;
        }
        *slot = Some(tauri::async_runtime::spawn(forward_events(app.clone())));
    }

    /// Follows a container's logs into `output`. Returns a token for
    /// `stop_logs`.
    pub fn follow_logs(&self, client: Client, id: String, output: Channel<LogMessage>) -> u32 {
        let token = self.next_log.fetch_add(1, Ordering::Relaxed);
        let task = tauri::async_runtime::spawn(async move {
            let sink = |lines: Vec<String>| match output.send(LogMessage::Lines { lines }) {
                Ok(()) => ControlFlow::Continue(()),
                Err(_) => ControlFlow::Break(()),
            };
            let outcome = client.follow_logs(&id, sink).await;
            // Without this a log that ended, or never started, looks like
            // one that is merely quiet.
            let error = outcome.err().map(|err| err.to_string());
            let _ = output.send(LogMessage::Ended { error });
        });
        let mut logs = self.logs.lock().expect("logs lock");
        // A log that ended on its own, because its container was removed,
        // would otherwise stay here until the UI stops it.
        logs.retain(|_, running| !running.inner().is_finished());
        logs.insert(token, task);
        token
    }

    pub fn stop_logs(&self, token: u32) {
        if let Some(task) = self.logs.lock().expect("logs lock").remove(&token) {
            task.abort();
        }
    }

    /// Ends every followed log.
    ///
    /// The page that asked for them cannot be relied on to stop them: a
    /// window that closes or reloads runs no cleanup, and sending to its
    /// channel keeps succeeding after the webview is gone.
    pub fn stop_all_logs(&self) {
        for (_, task) in self.logs.lock().expect("logs lock").drain() {
            task.abort();
        }
    }

    /// Ends the event subscription and every followed log.
    pub fn stop_all(&self) {
        if let Some(task) = self.events.lock().expect("events lock").take() {
            task.abort();
        }
        self.stop_all_logs();
    }
}

/// Forwards Docker's events to the UI and opens the stream again whenever it
/// ends, which it does when the engine is restarted from the CLI, the pipe
/// hiccups or an event cannot be decoded. Runs until it is aborted.
///
/// Every round ends in a sleep of at least a second, whether the stream ran,
/// failed at once or was not tried.
async fn forward_events(app: AppHandle) {
    let mut backoff = Backoff::default();
    let mut connected_before = false;
    loop {
        if engine_reachable().await
            && let Ok(client) = Client::connect()
        {
            if connected_before {
                // Changes made while nothing was listening were missed.
                let _ = app.emit(DOCKER_EVENT, "reconnecting");
            }
            connected_before = true;
            let opened = Instant::now();
            let forward = |kind: String| {
                let _ = app.emit(DOCKER_EVENT, kind);
            };
            let _ = client.watch_events(forward).await;
            // The stream ended: the engine stopped or the pipe went away.
            let _ = app.emit(DOCKER_EVENT, "disconnected");
            if opened.elapsed() >= STABLE_STREAM {
                backoff.reset();
            }
        }
        backoff.wait().await;
    }
}

/// See `supervisor::reachable`. Counts as unreachable if the check fails.
async fn engine_reachable() -> bool {
    tauri::async_runtime::spawn_blocking(supervisor::reachable)
        .await
        .unwrap_or(false)
}
