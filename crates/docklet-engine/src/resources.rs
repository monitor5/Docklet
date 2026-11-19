//! The Docker API calls behind resource usage, disk usage, bulk removal and
//! the prunes. The arithmetic and the result folding they use are in
//! `stats`, `disk` and `remove`.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;

use bollard::models::ContainerStatsResponse;
use bollard::query_parameters::{
    DataUsageOptionsBuilder, ListContainersOptionsBuilder, PruneContainersOptions,
    PruneImagesOptionsBuilder, PruneNetworksOptions, PruneVolumesOptionsBuilder,
    RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder, RemoveVolumeOptions,
    StatsOptionsBuilder,
};
use futures_util::future;
use futures_util::stream::{self, StreamExt};
use tokio::time::MissedTickBehavior;

use crate::disk::DiskUsage;
use crate::remove::{
    PruneReport, RemoveFailure, RemoveReport, RemoveResult, ResourceKind, Selection,
    containers_using, is_multi_tag_conflict, unique_ids,
};
use crate::stats::{ContainerUsage, UsageTracker};
use crate::{Client, Error};

/// How often `follow_usage` reports.
const USAGE_INTERVAL: Duration = Duration::from_secs(1);
/// Stats requests in flight at once. Each one needs a connection, and each
/// connection to the engine pipe is a relay process in the distro.
const STATS_CONCURRENCY: usize = 4;
/// Removals in flight at once, for the same reason.
const REMOVE_CONCURRENCY: usize = 4;

impl Client {
    /// Follows the usage of all running containers, giving `sink` a batch of
    /// rows (see `ContainerUsage`) about once a second, sorted by name.
    ///
    /// Each round lists the running containers and takes one sample of each,
    /// so containers that start later show up in the next batch and ones that
    /// stop drop out of it. CPU is measured between two rounds, so a
    /// container's first row has no CPU figure. `sink` ends the following by
    /// breaking; an engine that cannot be listed ends it with an error, after
    /// which the caller may try again with a `Backoff`.
    ///
    /// Every round waits for its tick, so this never spins, however quickly
    /// the engine answers or fails.
    pub async fn follow_usage(
        &self,
        mut sink: impl FnMut(Vec<ContainerUsage>) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let mut tracker = UsageTracker::default();
        let mut ticks = tokio::time::interval(USAGE_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let samples = self.usage_samples().await?;
            if sink(tracker.update(samples)).is_break() {
                return Ok(());
            }
        }
    }

    /// One one-shot sample of each running container. One-shot samples come
    /// back at once instead of after a second, and carry no `precpu_stats`;
    /// `UsageTracker` keeps the previous counters instead. A container that
    /// stops between the listing and its sample is left out.
    async fn usage_samples(&self) -> Result<Vec<ContainerStatsResponse>, Error> {
        let filters = HashMap::from([("status", vec!["running"])]);
        let options = ListContainersOptionsBuilder::new()
            .filters(&filters)
            .build();
        let running = self.docker.list_containers(Some(options)).await?;
        let containers = running.into_iter().filter_map(|container| {
            let name = container.names.and_then(|names| names.into_iter().next());
            container.id.map(|id| (id, name))
        });
        let samples = stream::iter(containers)
            .map(|(id, name)| async move {
                let options = StatsOptionsBuilder::new()
                    .stream(false)
                    .one_shot(true)
                    .build();
                let mut stats = pin!(self.docker.stats(&id, Some(options)));
                let mut sample = stats.next().await?.ok()?;
                sample.id.get_or_insert_with(|| id.clone());
                if sample.name.is_none() {
                    sample.name = name;
                }
                Some(sample)
            })
            .buffer_unordered(STATS_CONCURRENCY)
            .filter_map(future::ready)
            .collect()
            .await;
        Ok(samples)
    }

    /// What `docker system df -v` shows. The engine walks every layer and
    /// volume for this, which takes seconds on a big engine: call it when the
    /// user asks, never on a timer.
    pub async fn disk_usage(&self) -> Result<DiskUsage, Error> {
        let options = DataUsageOptionsBuilder::new().verbose(true).build();
        let response = self.docker.df(Some(options)).await?;
        Ok(DiskUsage::from_response(&response))
    }

    /// Removes each of `ids` (IDs or names) of one kind, a few at a time, and
    /// reports on every one in the order given; a failure does not stop the
    /// rest. Repeated IDs are removed once.
    ///
    /// - Containers are removed with force, killing them if they run, as
    ///   `container_action(Remove)` does. Their anonymous volumes stay.
    /// - Images are removed without force, so one used by a container, or
    ///   with child images, is reported and kept. An image named by tag loses
    ///   that tag only, as with `docker rmi <tag>`. An image named by ID that
    ///   several tags point at would need force; it is removed with force,
    ///   all tags together, only after checking that no container (stopped
    ///   ones included, which force would not protect) uses it.
    /// - Volumes and networks are removed as `docker volume rm` and `docker
    ///   network rm` do; one that a container uses is reported in use.
    pub async fn remove_many<S: AsRef<str>>(&self, kind: ResourceKind, ids: &[S]) -> RemoveReport {
        let ids = unique_ids(ids);
        let mut results: Vec<(usize, RemoveResult)> = stream::iter(ids.into_iter().enumerate())
            .map(|(index, id)| async move { (index, self.remove_one(kind, id).await) })
            .buffer_unordered(REMOVE_CONCURRENCY)
            .collect()
            .await;
        results.sort_by_key(|(index, _)| *index);
        RemoveReport {
            results: results.into_iter().map(|(_, result)| result).collect(),
        }
    }

    /// Removes a selection of several kinds: containers first, since they
    /// hold on to the rest, then images, volumes and networks. The report
    /// lists the results in that order.
    pub async fn remove_selection(&self, selection: &Selection) -> RemoveReport {
        let mut report = RemoveReport::default();
        for kind in ResourceKind::REMOVAL_ORDER {
            let ids = selection.ids(kind);
            if !ids.is_empty() {
                report.extend(self.remove_many(kind, ids).await);
            }
        }
        report
    }

    async fn remove_one(&self, kind: ResourceKind, id: String) -> RemoveResult {
        let docker = &self.docker;
        let outcome = match kind {
            ResourceKind::Container => {
                let options = RemoveContainerOptionsBuilder::new().force(true).build();
                docker.remove_container(&id, Some(options)).await
            }
            ResourceKind::Image => return self.remove_image_with_its_tags(id).await,
            ResourceKind::Volume => docker.remove_volume(&id, None::<RemoveVolumeOptions>).await,
            ResourceKind::Network => docker.remove_network(&id).await,
        };
        RemoveResult::from_outcome(kind, id, outcome)
    }

    /// See `remove_many` for why force is used only after a check.
    async fn remove_image_with_its_tags(&self, id: String) -> RemoveResult {
        let kind = ResourceKind::Image;
        let plain = RemoveImageOptionsBuilder::new().build();
        let outcome = self.docker.remove_image(&id, Some(plain), None).await;
        match outcome {
            Err(error) if is_multi_tag_conflict(&error) => {}
            other => return RemoveResult::from_outcome(kind, id, other.map(drop)),
        }
        let users = match self.image_users(&id).await {
            Ok(users) => users,
            Err(error) => return RemoveResult::from_outcome(kind, id, Err(error)),
        };
        match users {
            Some(users) if users.is_empty() => {
                let forced = RemoveImageOptionsBuilder::new().force(true).build();
                let outcome = self.docker.remove_image(&id, Some(forced), None).await;
                RemoveResult::from_outcome(kind, id, outcome.map(drop))
            }
            Some(users) => {
                let message = format!("image is used by container {}", users.join(", "));
                RemoveResult::failed(kind, id, RemoveFailure::InUse, message)
            }
            None => {
                let message = "the image's ID could not be read to check its containers";
                RemoveResult::failed(kind, id, RemoveFailure::Other, message)
            }
        }
    }

    /// The containers created from an image, or `None` when the engine did
    /// not say what the image's full ID is.
    async fn image_users(
        &self,
        image: &str,
    ) -> Result<Option<Vec<String>>, bollard::errors::Error> {
        let image_id = self.docker.inspect_image(image).await?.id;
        let Some(image_id) = image_id.filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let options = ListContainersOptionsBuilder::new().all(true).build();
        let containers = self.docker.list_containers(Some(options)).await?;
        Ok(Some(containers_using(&image_id, &containers)))
    }

    /// Removes all stopped containers, as `docker container prune`.
    pub async fn prune_containers(&self) -> Result<PruneReport, Error> {
        let options: Option<PruneContainersOptions> = None;
        let response = self.docker.prune_containers(options).await?;
        Ok(PruneReport::new(
            response.containers_deleted,
            response.space_reclaimed,
        ))
    }

    /// Removes dangling images, or with `all` every image no container uses,
    /// as `docker image prune [--all]`.
    pub async fn prune_images(&self, all: bool) -> Result<PruneReport, Error> {
        let mut builder = PruneImagesOptionsBuilder::new();
        if all {
            builder = builder.filters(&HashMap::from([("dangling", vec!["false"])]));
        }
        let response = self.docker.prune_images(Some(builder.build())).await?;
        Ok(PruneReport::of_images(response))
    }

    /// Removes unused anonymous volumes, or with `all` named ones too, as
    /// `docker volume prune [--all]`.
    pub async fn prune_volumes(&self, all: bool) -> Result<PruneReport, Error> {
        let mut builder = PruneVolumesOptionsBuilder::new();
        if all {
            builder = builder.filters(&HashMap::from([("all", vec!["true"])]));
        }
        let response = self.docker.prune_volumes(Some(builder.build())).await?;
        Ok(PruneReport::new(
            response.volumes_deleted,
            response.space_reclaimed,
        ))
    }

    /// Removes networks no container uses, as `docker network prune`.
    pub async fn prune_networks(&self) -> Result<PruneReport, Error> {
        let options: Option<PruneNetworksOptions> = None;
        let response = self.docker.prune_networks(options).await?;
        Ok(PruneReport::new(response.networks_deleted, None))
    }
}
