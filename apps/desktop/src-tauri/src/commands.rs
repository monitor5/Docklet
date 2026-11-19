//! Tauri commands: thin wrappers over `docklet-app` (engine lifecycle and
//! streams) and `docklet-engine` (the Docker API).
//!
//! Docker data is returned as bollard's own models, so the frontend sees the
//! Docker API's JSON field names (`Id`, `Names`, `State`, ...).

use std::io;

use docklet_core::EngineState;
use docklet_engine::models::{ContainerSummary, ImageSummary, Network, Volume};
use docklet_engine::{Client, ContainerAction};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::state::AppState;
use crate::streams::{self, LogMessage};
use crate::supervisor;

type CommandResult<T> = Result<T, String>;

fn text(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn client() -> CommandResult<Client> {
    Client::connect().map_err(text)
}

/// Runs blocking engine work (it shells out to `wsl.exe`) off the UI thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(text)?
        .map_err(text)
}

#[derive(Serialize)]
pub struct EngineStatus {
    #[serde(flatten)]
    state: EngineState,
    bases: Vec<&'static str>,
    /// The user's `.wslconfig` turns on mirrored networking, under which
    /// published ports may not reach Windows.
    mirrored_networking: bool,
}

impl From<docklet_app::EngineStatus> for EngineStatus {
    fn from(status: docklet_app::EngineStatus) -> Self {
        Self {
            state: status.state,
            bases: status.bases,
            mirrored_networking: status.mirrored_networking,
        }
    }
}

#[tauri::command]
pub async fn engine_status(app: AppHandle) -> CommandResult<EngineStatus> {
    let status = blocking(move || supervisor::status(&app)).await?;
    Ok(status.into())
}

/// Installs the distro, reporting each step through `progress`.
#[tauri::command]
pub async fn engine_install(
    app: AppHandle,
    base: Option<String>,
    progress: Channel<String>,
) -> CommandResult<()> {
    blocking(move || {
        supervisor::install(&app, base, &mut |step: &str| {
            let _ = progress.send(step.to_string());
        })
    })
    .await
}

#[tauri::command]
pub async fn engine_start(app: AppHandle) -> CommandResult<()> {
    blocking(move || supervisor::start(&app)).await
}

#[tauri::command]
pub async fn engine_stop(app: AppHandle) -> CommandResult<()> {
    blocking(move || supervisor::stop(&app)).await
}

#[tauri::command]
pub async fn events_watch(app: AppHandle) -> CommandResult<()> {
    streams::watch_events(&app);
    Ok(())
}

#[tauri::command]
pub async fn containers() -> CommandResult<Vec<ContainerSummary>> {
    client()?.containers().await.map_err(text)
}

#[tauri::command]
pub async fn container_action(id: String, action: String) -> CommandResult<()> {
    let action: ContainerAction = action.parse().map_err(text)?;
    let client = client()?;
    client.container_action(&id, action).await.map_err(text)
}

#[tauri::command]
pub async fn images() -> CommandResult<Vec<ImageSummary>> {
    client()?.images().await.map_err(text)
}

#[tauri::command]
pub async fn image_remove(id: String) -> CommandResult<()> {
    client()?.remove_image(&id).await.map_err(text)
}

/// Pulls an image, reporting each progress line through `progress`.
#[tauri::command]
pub async fn image_pull(name: String, progress: Channel<String>) -> CommandResult<()> {
    let report = move |line: String| {
        let _ = progress.send(line);
    };
    client()?.pull_image(&name, report).await.map_err(text)
}

#[tauri::command]
pub async fn volumes() -> CommandResult<Vec<Volume>> {
    client()?.volumes().await.map_err(text)
}

#[tauri::command]
pub async fn volume_remove(name: String) -> CommandResult<()> {
    client()?.remove_volume(&name).await.map_err(text)
}

#[tauri::command]
pub async fn networks() -> CommandResult<Vec<Network>> {
    client()?.networks().await.map_err(text)
}

#[tauri::command]
pub async fn network_remove(id: String) -> CommandResult<()> {
    client()?.remove_network(&id).await.map_err(text)
}

/// Opens a published port in the browser. The URL is built here from the
/// port number alone, so the webview cannot make the app open anything else.
#[tauri::command]
pub async fn open_port(port: u16) -> CommandResult<()> {
    tauri_plugin_opener::open_url(format!("http://localhost:{port}"), None::<&str>).map_err(text)
}

/// Follows a container's logs. Lines are sent in batches at most every 50 ms
/// so a chatty container cannot flood the UI, and a last message says how the
/// log ended. Returns a token for `logs_stop`.
#[tauri::command]
pub async fn logs_start(
    app: AppHandle,
    id: String,
    output: Channel<LogMessage>,
) -> CommandResult<u32> {
    Ok(streams::follow_logs(&app, client()?, id, output))
}

#[tauri::command]
pub async fn logs_stop(state: State<'_, AppState>, token: u32) -> CommandResult<()> {
    state.backend.streams().stop_logs(token);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_json(state: EngineState) -> serde_json::Value {
        let status = EngineStatus {
            state,
            bases: vec!["debian-13"],
            mirrored_networking: false,
        };
        serde_json::to_value(status).unwrap()
    }

    /// The names the TypeScript `EngineState` type is written against.
    #[test]
    fn the_window_gets_the_state_by_its_snake_case_name() {
        let names = [
            (EngineState::WslMissing, "wsl_missing"),
            (EngineState::NotInstalled, "not_installed"),
            (EngineState::Stopped, "stopped"),
            (EngineState::Sleeping, "sleeping"),
            (EngineState::Starting, "starting"),
            (EngineState::Running, "running"),
            (EngineState::Stopping, "stopping"),
        ];
        for (state, name) in names {
            let json = status_json(state);
            assert_eq!(json["state"], name);
            assert!(json.get("reason").is_none());
            assert_eq!(json["bases"][0], "debian-13");
        }
        let error = status_json(EngineState::Error("boom".to_string()));
        assert_eq!(error["state"], "error");
        assert_eq!(error["reason"], "boom");
    }
}
