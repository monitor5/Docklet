//! Removing many containers, images, volumes or networks in one go, and the
//! four prunes.
//!
//! A bulk removal never stops at the first failure: every item gets a
//! result, so a UI can say "removed 7, 2 are in use".

use std::fmt;

/// What a bulk removal works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Container,
    Image,
    Volume,
    Network,
}

impl ResourceKind {
    /// The order in which a mixed selection is removed: containers first,
    /// since they hold on to images, volumes and networks; networks last.
    pub const REMOVAL_ORDER: [Self; 4] =
        [Self::Container, Self::Image, Self::Volume, Self::Network];

    pub fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Image => "image",
            Self::Volume => "volume",
            Self::Network => "network",
        }
    }
}

/// Why an item was not removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoveFailure {
    /// A container uses the image, volume or network.
    InUse,
    /// Other images are built on top of the image.
    HasDependents,
    /// Nothing by that name or ID; it may be gone already.
    NotFound,
    /// The engine will not remove it at all, like the predefined networks.
    Refused,
    /// Anything else, including an engine that could not be reached.
    Other,
}

impl RemoveFailure {
    /// Lower-case words for a summary line.
    pub fn describe(self) -> &'static str {
        match self {
            Self::InUse => "in use",
            Self::HasDependents => "have dependent images",
            Self::NotFound => "not found",
            Self::Refused => "cannot be removed",
            Self::Other => "failed",
        }
    }

    /// Sorts a Docker API error answer into a reason, from its HTTP status
    /// and message.
    ///
    /// - 404: not found.
    /// - 409 conflict: "image has dependent child images" is a dependent,
    ///   any other conflict (used by a container, volume in use) is in use.
    /// - 403 forbidden: a network with active endpoints is in use; anything
    ///   else (a predefined network) is refused.
    /// - Anything else that says "in use" or "being used" is in use.
    pub fn classify(status: u16, message: &str) -> Self {
        let message = message.to_ascii_lowercase();
        let in_use = message.contains("in use")
            || message.contains("being used")
            || message.contains("active endpoints");
        match status {
            404 => Self::NotFound,
            409 if message.contains("child image") => Self::HasDependents,
            409 => Self::InUse,
            403 if in_use => Self::InUse,
            403 => Self::Refused,
            _ if in_use => Self::InUse,
            _ => Self::Other,
        }
    }

    /// The reason for any error a removal call can end with.
    pub fn of(error: &bollard::errors::Error) -> Self {
        match error {
            bollard::errors::Error::DockerResponseServerError {
                status_code,
                message,
            } => Self::classify(*status_code, message),
            _ => Self::Other,
        }
    }
}

/// What happened to one item of a bulk removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveResult {
    pub kind: ResourceKind,
    /// The ID or name as the caller gave it.
    pub id: String,
    /// `None` when the item was removed; otherwise why not, with the
    /// engine's message.
    pub failure: Option<(RemoveFailure, String)>,
}

impl RemoveResult {
    pub fn removed(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            failure: None,
        }
    }

    pub fn failed(
        kind: ResourceKind,
        id: impl Into<String>,
        reason: RemoveFailure,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            failure: Some((reason, message.into())),
        }
    }

    /// The result of one removal call.
    pub fn from_outcome(
        kind: ResourceKind,
        id: impl Into<String>,
        outcome: Result<(), bollard::errors::Error>,
    ) -> Self {
        match outcome {
            Ok(()) => Self::removed(kind, id),
            Err(error) => {
                let message = match &error {
                    bollard::errors::Error::DockerResponseServerError { message, .. } => {
                        message.clone()
                    }
                    other => other.to_string(),
                };
                Self::failed(kind, id, RemoveFailure::of(&error), message)
            }
        }
    }

    pub fn is_removed(&self) -> bool {
        self.failure.is_none()
    }

    pub fn reason(&self) -> Option<RemoveFailure> {
        self.failure.as_ref().map(|(reason, _)| *reason)
    }
}

/// The results of a bulk removal, one per item, in the order the items were
/// given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoveReport {
    pub results: Vec<RemoveResult>,
}

impl RemoveReport {
    pub fn removed(&self) -> usize {
        self.results.iter().filter(|r| r.is_removed()).count()
    }

    pub fn failed(&self) -> usize {
        self.results.len() - self.removed()
    }

    /// Items that failed for `reason`.
    pub fn count(&self, reason: RemoveFailure) -> usize {
        let reasons = self.results.iter().filter_map(RemoveResult::reason);
        reasons.filter(|r| *r == reason).count()
    }

    /// Adds the results of another removal after these.
    pub fn extend(&mut self, other: Self) {
        self.results.extend(other.results);
    }
}

/// "removed 7, 2 in use, 1 not found": the removed count, then each reason
/// that occurred, in a fixed order.
impl fmt::Display for RemoveReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "removed {}", self.removed())?;
        for reason in [
            RemoveFailure::InUse,
            RemoveFailure::HasDependents,
            RemoveFailure::NotFound,
            RemoveFailure::Refused,
            RemoveFailure::Other,
        ] {
            let count = self.count(reason);
            if count > 0 {
                write!(f, ", {count} {}", reason.describe())?;
            }
        }
        Ok(())
    }
}

/// Items of several kinds to remove together, for `Client::remove_selection`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub containers: Vec<String>,
    pub images: Vec<String>,
    pub volumes: Vec<String>,
    pub networks: Vec<String>,
}

impl Selection {
    pub fn ids(&self, kind: ResourceKind) -> &[String] {
        match kind {
            ResourceKind::Container => &self.containers,
            ResourceKind::Image => &self.images,
            ResourceKind::Volume => &self.volumes,
            ResourceKind::Network => &self.networks,
        }
    }

    pub fn is_empty(&self) -> bool {
        ResourceKind::REMOVAL_ORDER
            .iter()
            .all(|&kind| self.ids(kind).is_empty())
    }
}

/// Names (or short IDs) of the containers, running or not, created from the
/// image with the full ID `image_id`.
pub(crate) fn containers_using(
    image_id: &str,
    containers: &[bollard::models::ContainerSummary],
) -> Vec<String> {
    containers
        .iter()
        .filter(|container| container.image_id.as_deref() == Some(image_id))
        .map(|container| {
            let name = container.names.as_ref().and_then(|names| names.first());
            match name {
                Some(name) => name.trim_start_matches('/').to_string(),
                None => container
                    .id
                    .as_deref()
                    .unwrap_or_default()
                    .chars()
                    .take(12)
                    .collect(),
            }
        })
        .collect()
}

/// The IDs in the order given, each once.
pub(crate) fn unique_ids<S: AsRef<str>>(ids: &[S]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .map(|id| id.as_ref().trim().to_string())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// True for the conflict the engine reports when an image is removed by ID
/// while several names point at it.
pub(crate) fn is_multi_tag_conflict(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError { status_code: 409, message }
            if message.contains("referenced in multiple repositories")
    )
}

/// What a prune deleted and freed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// IDs (or, for volumes, names) of what was deleted.
    pub deleted: Vec<String>,
    /// Image names that were untagged; empty for the other kinds.
    pub untagged: Vec<String>,
    /// Bytes freed; always zero for networks.
    pub space_reclaimed: u64,
}

impl PruneReport {
    pub(crate) fn new(deleted: Option<Vec<String>>, space_reclaimed: Option<i64>) -> Self {
        Self {
            deleted: deleted.unwrap_or_default(),
            untagged: Vec::new(),
            space_reclaimed: space_reclaimed
                .and_then(|n| u64::try_from(n).ok())
                .unwrap_or(0),
        }
    }

    pub(crate) fn of_images(response: bollard::models::ImagePruneResponse) -> Self {
        let mut report = Self::new(None, response.space_reclaimed);
        for item in response.images_deleted.into_iter().flatten() {
            report.deleted.extend(item.deleted);
            report.untagged.extend(item.untagged);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_error(status_code: u16, message: &str) -> bollard::errors::Error {
        bollard::errors::Error::DockerResponseServerError {
            status_code,
            message: message.to_string(),
        }
    }

    #[test]
    fn engine_messages_sort_into_reasons() {
        use RemoveFailure::*;
        let cases = [
            (404, "No such container: web", NotFound),
            (404, "No such image: nginx:latest", NotFound),
            (404, "get data: no such volume", NotFound),
            (
                409,
                "conflict: unable to delete 1a2b3c4d5e6f (must be forced) - image is being used by stopped container 9f8e7d6c5b4a",
                InUse,
            ),
            (
                409,
                "conflict: unable to delete 1a2b3c4d5e6f (cannot be forced) - image is being used by running container 9f8e7d6c5b4a",
                InUse,
            ),
            (
                409,
                "conflict: unable to delete 1a2b3c4d5e6f (cannot be forced) - image has dependent child images",
                HasDependents,
            ),
            (
                409,
                "remove data: volume is in use - [9f8e7d6c5b4a0d1e]",
                InUse,
            ),
            (
                403,
                "error while removing network: network app_default id 3c2b1a has active endpoints",
                InUse,
            ),
            (
                403,
                "bridge is a pre-defined network and cannot be removed",
                Refused,
            ),
            (500, "driver failed: volume is in use", InUse),
            (500, "something broke", Other),
        ];
        for (status, message, expected) in cases {
            assert_eq!(
                RemoveFailure::classify(status, message),
                expected,
                "{status} {message}"
            );
        }
    }

    #[test]
    fn a_failed_call_keeps_the_engines_message() {
        let result = RemoveResult::from_outcome(
            ResourceKind::Volume,
            "data",
            Err(server_error(409, "volume is in use - [abc]")),
        );
        assert!(!result.is_removed());
        assert_eq!(result.reason(), Some(RemoveFailure::InUse));
        assert_eq!(
            result.failure.unwrap().1,
            "volume is in use - [abc]".to_string()
        );

        let removed = RemoveResult::from_outcome(ResourceKind::Network, "n1", Ok(()));
        assert!(removed.is_removed());
        assert_eq!(removed.reason(), None);
    }

    #[test]
    fn transport_errors_are_other() {
        let error = bollard::errors::Error::RequestTimeoutError;
        let result = RemoveResult::from_outcome(ResourceKind::Container, "c1", Err(error));
        assert_eq!(result.reason(), Some(RemoveFailure::Other));
        assert!(!result.failure.unwrap().1.is_empty());
    }

    #[test]
    fn the_report_counts_and_summarises() {
        use RemoveFailure::*;
        let mut report = RemoveReport::default();
        for i in 0..7 {
            report
                .results
                .push(RemoveResult::removed(ResourceKind::Image, format!("i{i}")));
        }
        report.extend(RemoveReport {
            results: vec![
                RemoveResult::failed(ResourceKind::Image, "a", InUse, "used"),
                RemoveResult::failed(ResourceKind::Image, "b", InUse, "used"),
                RemoveResult::failed(ResourceKind::Image, "c", NotFound, "gone"),
            ],
        });
        assert_eq!(report.removed(), 7);
        assert_eq!(report.failed(), 3);
        assert_eq!(report.count(InUse), 2);
        assert_eq!(report.count(HasDependents), 0);
        assert_eq!(report.to_string(), "removed 7, 2 in use, 1 not found");
        assert_eq!(RemoveReport::default().to_string(), "removed 0");
    }

    #[test]
    fn ids_are_deduplicated_in_order() {
        let ids = ["b", "a", " b ", "", "c", "a"];
        assert_eq!(unique_ids(&ids), ["b", "a", "c"]);
    }

    #[test]
    fn only_the_multi_tag_conflict_is_retried() {
        let multi = server_error(
            409,
            "conflict: unable to delete 1a2b3c4d5e6f (must be forced) - image is referenced in multiple repositories",
        );
        assert!(is_multi_tag_conflict(&multi));
        let used = server_error(
            409,
            "conflict: unable to delete 1a2b3c4d5e6f (must be forced) - image is being used by stopped container 9f8e",
        );
        assert!(!is_multi_tag_conflict(&used));
        assert!(!is_multi_tag_conflict(&server_error(
            404,
            "referenced in multiple repositories"
        )));
    }

    #[test]
    fn containers_using_an_image_are_named() {
        let containers: Vec<bollard::models::ContainerSummary> = serde_json::from_str(
            r#"[
                { "Id": "c1c1c1c1c1c1c1c1", "Names": ["/web"], "ImageID": "sha256:aaa", "State": "running" },
                { "Id": "c2c2c2c2c2c2c2c2", "Names": [], "ImageID": "sha256:aaa", "State": "exited" },
                { "Id": "c3c3c3c3c3c3c3c3", "Names": ["/db"], "ImageID": "sha256:bbb", "State": "exited" }
            ]"#,
        )
        .unwrap();
        assert_eq!(
            containers_using("sha256:aaa", &containers),
            ["web", "c2c2c2c2c2c2"]
        );
        assert!(containers_using("sha256:ccc", &containers).is_empty());
    }

    #[test]
    fn a_selection_lists_its_kinds() {
        let selection = Selection {
            images: vec!["nginx".into()],
            networks: vec!["n1".into(), "n2".into()],
            ..Default::default()
        };
        assert!(!selection.is_empty());
        assert!(selection.ids(ResourceKind::Container).is_empty());
        assert_eq!(selection.ids(ResourceKind::Network), ["n1", "n2"]);
        assert!(Selection::default().is_empty());
        // Containers go first: they hold on to everything else.
        assert_eq!(ResourceKind::REMOVAL_ORDER[0], ResourceKind::Container);
        assert_eq!(ResourceKind::REMOVAL_ORDER[3], ResourceKind::Network);
    }

    #[test]
    fn prune_answers_become_reports() {
        let report = PruneReport::new(Some(vec!["c1".into(), "c2".into()]), Some(4096));
        assert_eq!(report.deleted, ["c1", "c2"]);
        assert_eq!(report.space_reclaimed, 4096);
        assert_eq!(PruneReport::new(None, Some(-1)), PruneReport::default());

        let images: bollard::models::ImagePruneResponse = serde_json::from_str(
            r#"{
                "ImagesDeleted": [
                    { "Untagged": "nginx:latest" },
                    { "Untagged": "nginx@sha256:0f0f" },
                    { "Deleted": "sha256:aaa" },
                    { "Deleted": "sha256:bbb" }
                ],
                "SpaceReclaimed": 190000000
            }"#,
        )
        .unwrap();
        let report = PruneReport::of_images(images);
        assert_eq!(report.deleted, ["sha256:aaa", "sha256:bbb"]);
        assert_eq!(report.untagged, ["nginx:latest", "nginx@sha256:0f0f"]);
        assert_eq!(report.space_reclaimed, 190_000_000);
    }
}
