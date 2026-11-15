//! Tauri commands: thin wrappers over the supervisor (engine lifecycle) and
//! `docklet-engine` (the Docker API).
//!
//! Docker data is returned as bollard's own models, so the frontend sees the
//! Docker API's JSON field names (`Id`, `Names`, `State`, ...).

use std::io;

use docklet_core::{EngineState, engine};
use docklet_engine::models::{ContainerSummary, ImageSummary, Network, Volume};
use docklet_engine::{Client, ContainerAction};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::state::AppState;
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
}

#[tauri::command]
pub async fn engine_status() -> CommandResult<EngineStatus> {
    let state = blocking(supervisor::status).await?;
    Ok(EngineStatus {
        state,
        bases: engine::bases(),
    })
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
pub async fn events_watch(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    state.streams.watch_events(&app);
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

/// Follows a container's logs. Lines are sent in batches at most every 50 ms
/// so a chatty container cannot flood the UI. Returns a token for `logs_stop`.
#[tauri::command]
pub async fn logs_start(
    state: State<'_, AppState>,
    id: String,
    lines: Channel<Vec<String>>,
) -> CommandResult<u32> {
    Ok(state.streams.follow_logs(client()?, id, lines))
}

#[tauri::command]
pub async fn logs_stop(state: State<'_, AppState>, token: u32) -> CommandResult<()> {
    state.streams.stop_logs(token);
    Ok(())
}
