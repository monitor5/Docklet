//! The Docker API calls Docklet makes, over the engine pipe.

use std::ops::ControlFlow;
use std::pin::pin;
use std::str::FromStr;
use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerSummary, ImageSummary, Network, Volume};
use bollard::query_parameters::{
    CreateImageOptionsBuilder, EventsOptions, ListContainersOptionsBuilder, ListImagesOptions,
    ListNetworksOptions, ListVolumesOptions, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    RemoveImageOptionsBuilder, RemoveVolumeOptions, RestartContainerOptions, StartContainerOptions,
    StopContainerOptionsBuilder,
};
use futures_util::stream::StreamExt;

use crate::logs::batch_lines;
use crate::pull::progress_line;
use crate::{Error, ImageRef};

/// Seconds before a Docker API call that gets no answer is given up.
#[cfg(windows)]
const TIMEOUT_SECS: u64 = 120;
/// Log lines reach the caller at most this often.
const LOG_BATCH: Duration = Duration::from_millis(50);
/// Lines of existing output that a followed log starts with.
const LOG_TAIL: &str = "500";

/// What `Client::container_action` can do to a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    /// Removes the container, killing it first if it runs.
    Remove,
}

impl FromStr for ContainerAction {
    type Err = Error;

    fn from_str(action: &str) -> Result<Self, Error> {
        match action {
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "restart" => Ok(Self::Restart),
            "remove" => Ok(Self::Remove),
            other => Err(Error::UnknownAction(other.to_string())),
        }
    }
}

/// A client for the Docker Engine behind the Docklet pipe.
pub struct Client {
    pub(crate) docker: Docker,
}

impl Client {
    /// Returns a client for the engine pipe. Nothing is opened yet: a pipe
    /// that is not there shows up as an error from the first call.
    #[cfg(windows)]
    pub fn connect() -> Result<Self, Error> {
        let docker = Docker::connect_with_named_pipe(
            docklet_core::engine::ENGINE_PIPE,
            TIMEOUT_SECS,
            bollard::API_DEFAULT_VERSION,
        )?;
        Ok(Self { docker })
    }

    /// The engine pipe only exists on Windows.
    #[cfg(not(windows))]
    pub fn connect() -> Result<Self, Error> {
        Err(Error::Unsupported)
    }

    /// All containers, running or not.
    pub async fn containers(&self) -> Result<Vec<ContainerSummary>, Error> {
        let options = ListContainersOptionsBuilder::new().all(true).build();
        Ok(self.docker.list_containers(Some(options)).await?)
    }

    pub async fn container_action(&self, id: &str, action: ContainerAction) -> Result<(), Error> {
        let docker = &self.docker;
        match action {
            ContainerAction::Start => {
                let options: Option<StartContainerOptions> = None;
                docker.start_container(id, options).await?;
            }
            ContainerAction::Stop => {
                let options = StopContainerOptionsBuilder::new().t(5).build();
                docker.stop_container(id, Some(options)).await?;
            }
            ContainerAction::Restart => {
                let options: Option<RestartContainerOptions> = None;
                docker.restart_container(id, options).await?;
            }
            ContainerAction::Remove => {
                let options = RemoveContainerOptionsBuilder::new().force(true).build();
                docker.remove_container(id, Some(options)).await?;
            }
        }
        Ok(())
    }

    pub async fn images(&self) -> Result<Vec<ImageSummary>, Error> {
        let options: Option<ListImagesOptions> = None;
        Ok(self.docker.list_images(options).await?)
    }

    pub async fn remove_image(&self, id: &str) -> Result<(), Error> {
        let options = RemoveImageOptionsBuilder::new().build();
        self.docker.remove_image(id, Some(options), None).await?;
        Ok(())
    }

    /// Pulls an image given the way `docker pull` takes it, calling
    /// `on_progress` with a line for each update Docker sends.
    pub async fn pull_image(
        &self,
        reference: &str,
        mut on_progress: impl FnMut(String),
    ) -> Result<(), Error> {
        let image = ImageRef::parse(reference)?;
        let options = CreateImageOptionsBuilder::new()
            .from_image(image.name)
            .tag(image.tag)
            .build();
        let mut updates = pin!(self.docker.create_image(Some(options), None, None));
        while let Some(update) = updates.next().await {
            let info = update?;
            let progress = info
                .progress_detail
                .and_then(|detail| detail.current.zip(detail.total));
            let line = progress_line(info.id.as_deref(), info.status.as_deref(), progress);
            if !line.is_empty() {
                on_progress(line);
            }
        }
        Ok(())
    }

    pub async fn volumes(&self) -> Result<Vec<Volume>, Error> {
        let options: Option<ListVolumesOptions> = None;
        let response = self.docker.list_volumes(options).await?;
        Ok(response.volumes.unwrap_or_default())
    }

    pub async fn remove_volume(&self, name: &str) -> Result<(), Error> {
        let options: Option<RemoveVolumeOptions> = None;
        self.docker.remove_volume(name, options).await?;
        Ok(())
    }

    pub async fn networks(&self) -> Result<Vec<Network>, Error> {
        let options: Option<ListNetworksOptions> = None;
        Ok(self.docker.list_networks(options).await?)
    }

    pub async fn remove_network(&self, id: &str) -> Result<(), Error> {
        self.docker.remove_network(id).await?;
        Ok(())
    }

    /// Calls `on_event` with the kind of each Docker event (`container`,
    /// `image`, `volume`, ...) until the stream ends, which it does when the
    /// engine stops or the pipe goes away.
    pub async fn watch_events(&self, mut on_event: impl FnMut(String)) -> Result<(), Error> {
        let options: Option<EventsOptions> = None;
        let mut events = pin!(self.docker.events(options));
        while let Some(event) = events.next().await {
            let kind = event?.typ.map(|typ| typ.to_string());
            on_event(kind.unwrap_or_default());
        }
        Ok(())
    }

    /// Follows a container's logs, starting with its last 500 lines.
    ///
    /// `sink` receives the lines in batches, at most one every 50 ms, and
    /// ends the following by breaking. Otherwise this returns when the log
    /// ends, for example because the container was removed.
    pub async fn follow_logs(
        &self,
        id: &str,
        sink: impl FnMut(Vec<String>) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let options = LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .tail(LOG_TAIL)
            .build();
        let chunks = self.docker.logs(id, Some(options));
        batch_lines(chunks, LOG_BATCH, sink).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_parse_from_their_names() {
        let parse = |name: &str| name.parse::<ContainerAction>().unwrap();
        assert_eq!(parse("start"), ContainerAction::Start);
        assert_eq!(parse("stop"), ContainerAction::Stop);
        assert_eq!(parse("restart"), ContainerAction::Restart);
        assert_eq!(parse("remove"), ContainerAction::Remove);
    }

    #[test]
    fn unknown_action_is_an_error_that_names_it() {
        let err = "pause".parse::<ContainerAction>().unwrap_err();
        assert_eq!(err.to_string(), "unknown container action `pause`");
    }

    #[cfg(not(windows))]
    #[test]
    fn connecting_needs_windows() {
        assert!(matches!(Client::connect(), Err(Error::Unsupported)));
    }
}
