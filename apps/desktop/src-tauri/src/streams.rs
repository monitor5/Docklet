//! Long-lived Docker streams, adapted from `docklet-app` to the webview:
//! events become Tauri events, log lines go through a channel.

use std::ops::ControlFlow;
use std::sync::Arc;

use docklet_app::{Backend, DockerEvent, StreamSink, WindowState};
use docklet_engine::Client;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// Event sent to the UI whenever Docker reports a change, when the event
/// stream ends and when it is about to be opened again.
const DOCKER_EVENT: &str = "docker-event";

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

impl From<docklet_app::LogMessage> for LogMessage {
    fn from(message: docklet_app::LogMessage) -> Self {
        match message {
            docklet_app::LogMessage::Lines(lines) => Self::Lines { lines },
            docklet_app::LogMessage::Ended { error } => Self::Ended { error },
        }
    }
}

/// Sends what the event stream says to the webview: the kind of a Docker
/// event, `disconnected` or `reconnecting`.
struct Events(AppHandle);

impl StreamSink for Events {
    fn docker_event(&self, event: DockerEvent) {
        let _ = self.0.emit(DOCKER_EVENT, event.name());
    }
}

/// The engine control and streams of this app, on Tauri's own runtime.
pub fn backend(app: &AppHandle) -> Backend {
    let runtime = tauri::async_runtime::handle().inner().clone();
    Backend::system(runtime, Arc::new(Events(app.clone())))
}

/// Tells the streams whether there is a window right now. They start
/// nothing without one: nobody would listen, and nothing would stop them
/// when the window is already gone.
pub fn sync_window(app: &AppHandle) {
    let window = match app.get_webview_window(crate::MAIN_WINDOW) {
        Some(_) => WindowState::Visible,
        None => WindowState::Gone,
    };
    app.state::<AppState>().backend.streams().set_window(window);
}

/// Subscribes to Docker's event stream and forwards each event to the UI,
/// which refetches the affected list. The subscription is renewed when it
/// ends, until the engine is stopped or the window is gone. Idempotent.
/// Does nothing without a window; the next list view asks again.
pub fn watch_events(app: &AppHandle) {
    sync_window(app);
    app.state::<AppState>().backend.streams().watch_events();
}

/// Follows a container's logs into `output`. Returns a token for
/// `Streams::stop_logs`.
pub fn follow_logs(
    app: &AppHandle,
    client: Client,
    id: String,
    output: Channel<LogMessage>,
) -> u32 {
    // The page that asks is the window.
    sync_window(app);
    let send = move |message: docklet_app::LogMessage| match output.send(message.into()) {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(()),
    };
    let state = app.state::<AppState>();
    state.backend.streams().follow_logs(client, id, send)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(message: docklet_app::LogMessage) -> serde_json::Value {
        serde_json::to_value(LogMessage::from(message)).unwrap()
    }

    /// The shapes the TypeScript `LogMessage` type is written against.
    #[test]
    fn the_window_gets_log_messages_tagged_by_kind() {
        let lines = json(docklet_app::LogMessage::Lines(vec!["a".to_string()]));
        assert_eq!(lines["kind"], "lines");
        assert_eq!(lines["lines"][0], "a");

        let ended = json(docklet_app::LogMessage::Ended { error: None });
        assert_eq!(ended["kind"], "ended");
        assert!(ended["error"].is_null());

        let error = Some("no such container".to_string());
        let failed = json(docklet_app::LogMessage::Ended { error });
        assert_eq!(failed["error"], "no such container");
    }

    /// The payloads `hooks.ts` and `engine.ts` compare against.
    #[test]
    fn the_window_gets_events_by_their_one_word_name() {
        assert_eq!(DockerEvent::Changed("image".to_string()).name(), "image");
        assert_eq!(DockerEvent::Disconnected.name(), "disconnected");
        assert_eq!(DockerEvent::Reconnecting.name(), "reconnecting");
    }
}
