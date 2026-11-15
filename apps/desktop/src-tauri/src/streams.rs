//! Long-lived Docker streams, adapted from `docklet-engine` callbacks to the
//! webview: events become Tauri events, log lines go through a channel.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use docklet_engine::Client;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

/// Event sent to the UI whenever Docker reports a change, and when the event
/// stream ends.
const DOCKER_EVENT: &str = "docker-event";

/// The background tasks that feed the UI.
#[derive(Default)]
pub struct Streams {
    events: Mutex<Option<JoinHandle<()>>>,
    logs: Mutex<HashMap<u32, JoinHandle<()>>>,
    next_log: AtomicU32,
}

impl Streams {
    /// Subscribes to Docker's event stream and forwards each event to the UI,
    /// which refetches the affected list. Idempotent.
    pub fn watch_events(&self, app: &AppHandle) {
        let mut slot = self.events.lock().expect("events lock");
        if slot
            .as_ref()
            .is_some_and(|task| !task.inner().is_finished())
        {
            return;
        }
        let app = app.clone();
        *slot = Some(tauri::async_runtime::spawn(async move {
            if let Ok(client) = Client::connect() {
                let forward = |kind: String| {
                    let _ = app.emit(DOCKER_EVENT, kind);
                };
                let _ = client.watch_events(forward).await;
                // The stream ended: the engine stopped or the pipe went away.
                let _ = app.emit(DOCKER_EVENT, "disconnected");
            }
        }));
    }

    /// Follows a container's logs into `lines`. Returns a token for
    /// `stop_logs`.
    pub fn follow_logs(&self, client: Client, id: String, lines: Channel<Vec<String>>) -> u32 {
        let token = self.next_log.fetch_add(1, Ordering::Relaxed);
        let task = tauri::async_runtime::spawn(async move {
            let sink = move |batch: Vec<String>| match lines.send(batch) {
                Ok(()) => ControlFlow::Continue(()),
                // The webview that asked for these logs is gone.
                Err(_) => ControlFlow::Break(()),
            };
            let _ = client.follow_logs(&id, sink).await;
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

    /// Ends the event subscription and every followed log.
    pub fn stop_all(&self) {
        if let Some(task) = self.events.lock().expect("events lock").take() {
            task.abort();
        }
        for (_, task) in self.logs.lock().expect("logs lock").drain() {
            task.abort();
        }
    }
}
