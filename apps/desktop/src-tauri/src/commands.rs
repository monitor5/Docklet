//! Tauri commands: engine lifecycle and the Docker API.
//!
//! Docker data is returned as bollard's own models, so the frontend sees the
//! Docker API's JSON field names (`Id`, `Names`, `State`, ...).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bollard::models::{ContainerSummary, ImageSummary, Network, Volume};
use bollard::query_parameters::{
    CreateImageOptionsBuilder, ListContainersOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder, StopContainerOptionsBuilder,
};
use bollard::{API_DEFAULT_VERSION, Docker};
use docklet_core::{EngineState, engine};
use docklet_proxy::{ENGINE_PIPE, control};
use docklet_wsl::{Engine, RootfsSource};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

/// Event sent to the UI whenever Docker reports a change, and when the event
/// stream ends.
pub const DOCKER_EVENT: &str = "docker-event";
/// Event sent to the UI when the engine state changed from the tray.
pub const ENGINE_EVENT: &str = "engine-changed";

const DOCKER_TIMEOUT_SECS: u64 = 120;
const LOG_BATCH: Duration = Duration::from_millis(50);
const LOG_TAIL: &str = "500";

#[derive(Default)]
pub struct AppState {
    events: Mutex<Option<JoinHandle<()>>>,
    logs: Mutex<HashMap<u32, JoinHandle<()>>>,
    next_log: AtomicU32,
}

type CommandResult<T> = Result<T, String>;

fn text(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn docker() -> CommandResult<Docker> {
    Docker::connect_with_named_pipe(ENGINE_PIPE, DOCKER_TIMEOUT_SECS, API_DEFAULT_VERSION)
        .map_err(text)
}

/// Runs blocking engine work (it shells out to `wsl.exe`) off the UI thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
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

/// The engine counts as running for the UI only when the pipe proxy is up,
/// because that is what the UI itself talks to.
fn probe() -> std::io::Result<EngineState> {
    let state = Engine::system()?.status();
    Ok(match state {
        EngineState::Running if !control::is_running() => EngineState::Starting,
        other => other,
    })
}

#[tauri::command]
pub async fn engine_status() -> CommandResult<EngineStatus> {
    let state = blocking(probe).await?;
    Ok(EngineStatus {
        state,
        bases: engine::bases(),
    })
}

#[tauri::command]
pub async fn engine_install(base: Option<String>) -> CommandResult<()> {
    blocking(move || {
        let base = base.unwrap_or_else(|| engine::bases()[0].to_string());
        Engine::system()?.install(&RootfsSource::Base(base), &mut |_| {})
    })
    .await
}

/// Starts the distro, dockerd and the pipe proxy.
pub fn start_engine_blocking() -> std::io::Result<()> {
    Engine::system()?.start()?;
    if control::is_running() {
        return Ok(());
    }
    // The proxy is this same executable in `--proxy` mode, detached so the
    // Windows docker CLI keeps working when the GUI exits.
    let exe = std::env::current_exe()?;
    docklet_wsl::spawn_detached(&format!("\"{}\" --proxy", exe.display()))?;
    for _ in 0..100 {
        if control::is_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(std::io::Error::other("the pipe proxy did not start"))
}

pub fn stop_engine_blocking() -> std::io::Result<()> {
    control::request_stop();
    Engine::system()?.stop()
}

#[tauri::command]
pub async fn engine_start(app: AppHandle) -> CommandResult<()> {
    blocking(start_engine_blocking).await?;
    watch_events(&app);
    Ok(())
}

#[tauri::command]
pub async fn engine_stop(app: AppHandle) -> CommandResult<()> {
    stop_watching(&app);
    blocking(stop_engine_blocking).await
}

/// Subscribes to Docker's event stream and forwards each event to the UI,
/// which refetches the affected list. Idempotent.
pub fn watch_events(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut slot = state.events.lock().expect("events lock");
    if slot
        .as_ref()
        .is_some_and(|task| !task.inner().is_finished())
    {
        return;
    }
    let app = app.clone();
    *slot = Some(tauri::async_runtime::spawn(async move {
        let Ok(docker) = docker() else { return };
        let mut events = docker.events(None);
        while let Some(Ok(event)) = events.next().await {
            let kind = event.typ.map(|typ| typ.to_string()).unwrap_or_default();
            let _ = app.emit(DOCKER_EVENT, kind);
        }
        // The stream ended: the engine stopped or the pipe went away.
        let _ = app.emit(DOCKER_EVENT, "disconnected");
    }));
}

fn stop_watching(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Some(task) = state.events.lock().expect("events lock").take() {
        task.abort();
    }
    for (_, task) in state.logs.lock().expect("logs lock").drain() {
        task.abort();
    }
}

#[tauri::command]
pub async fn events_watch(app: AppHandle) -> CommandResult<()> {
    watch_events(&app);
    Ok(())
}

#[tauri::command]
pub async fn containers() -> CommandResult<Vec<ContainerSummary>> {
    let options = ListContainersOptionsBuilder::new().all(true).build();
    docker()?.list_containers(Some(options)).await.map_err(text)
}

#[tauri::command]
pub async fn container_action(id: String, action: String) -> CommandResult<()> {
    let docker = docker()?;
    match action.as_str() {
        "start" => docker.start_container(&id, None).await,
        "stop" => {
            let options = StopContainerOptionsBuilder::new().t(5).build();
            docker.stop_container(&id, Some(options)).await
        }
        "restart" => docker.restart_container(&id, None).await,
        "remove" => {
            let options = RemoveContainerOptionsBuilder::new().force(true).build();
            docker.remove_container(&id, Some(options)).await
        }
        other => return Err(format!("unknown container action `{other}`")),
    }
    .map_err(text)
}

#[tauri::command]
pub async fn images() -> CommandResult<Vec<ImageSummary>> {
    docker()?
        .list_images(None::<bollard::query_parameters::ListImagesOptions>)
        .await
        .map_err(text)
}

#[tauri::command]
pub async fn image_remove(id: String) -> CommandResult<()> {
    let options = RemoveImageOptionsBuilder::new().build();
    docker()?
        .remove_image(&id, Some(options), None)
        .await
        .map(drop)
        .map_err(text)
}

/// Pulls an image, reporting each progress line through `progress`.
#[tauri::command]
pub async fn image_pull(name: String, progress: Channel<String>) -> CommandResult<()> {
    let (image, tag) = match name.rsplit_once(':') {
        // A colon inside the last path segment separates the tag; one before
        // a slash belongs to a registry port.
        Some((image, tag)) if !tag.contains('/') => (image, tag),
        _ => (name.as_str(), "latest"),
    };
    let options = CreateImageOptionsBuilder::new()
        .from_image(image)
        .tag(tag)
        .build();
    let docker = docker()?;
    let mut stream = docker.create_image(Some(options), None, None);
    while let Some(update) = stream.next().await {
        let info = update.map_err(text)?;
        let progress_text = info.progress_detail.and_then(|detail| {
            let (current, total) = (detail.current?, detail.total?);
            (total > 0).then(|| format!("{}%", current * 100 / total))
        });
        let line = [info.id, info.status, progress_text]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let _ = progress.send(line);
    }
    Ok(())
}

#[tauri::command]
pub async fn volumes() -> CommandResult<Vec<Volume>> {
    docker()?
        .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
        .await
        .map(|response| response.volumes.unwrap_or_default())
        .map_err(text)
}

#[tauri::command]
pub async fn volume_remove(name: String) -> CommandResult<()> {
    docker()?
        .remove_volume(
            &name,
            None::<bollard::query_parameters::RemoveVolumeOptions>,
        )
        .await
        .map_err(text)
}

#[tauri::command]
pub async fn networks() -> CommandResult<Vec<Network>> {
    docker()?.list_networks(None).await.map_err(text)
}

#[tauri::command]
pub async fn network_remove(id: String) -> CommandResult<()> {
    docker()?.remove_network(&id).await.map_err(text)
}

/// Follows a container's logs. Lines are sent in batches at most every 50 ms
/// so a chatty container cannot flood the UI. Returns a token for `logs_stop`.
#[tauri::command]
pub async fn logs_start(
    state: State<'_, AppState>,
    id: String,
    lines: Channel<Vec<String>>,
) -> CommandResult<u32> {
    let docker = docker()?;
    let token = state.next_log.fetch_add(1, Ordering::Relaxed);
    let task = tauri::async_runtime::spawn(async move {
        let options = LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .tail(LOG_TAIL)
            .build();
        let mut stream = docker.logs(&id, Some(options));
        let mut batch: Vec<String> = Vec::new();
        let mut partial = String::new();
        let mut tick = tokio::time::interval(LOG_BATCH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(Ok(output)) => {
                        partial.push_str(&String::from_utf8_lossy(output.as_ref()));
                        while let Some(end) = partial.find('\n') {
                            let line: String = partial.drain(..=end).collect();
                            batch.push(line.trim_end_matches(['\r', '\n']).to_string());
                        }
                    }
                    _ => break,
                },
                _ = tick.tick() => {
                    if !batch.is_empty() && lines.send(std::mem::take(&mut batch)).is_err() {
                        return;
                    }
                }
            }
        }
        if !partial.is_empty() {
            batch.push(partial);
        }
        if !batch.is_empty() {
            let _ = lines.send(batch);
        }
    });
    state.logs.lock().expect("logs lock").insert(token, task);
    Ok(token)
}

#[tauri::command]
pub async fn logs_stop(state: State<'_, AppState>, token: u32) -> CommandResult<()> {
    if let Some(task) = state.logs.lock().expect("logs lock").remove(&token) {
        task.abort();
    }
    Ok(())
}
