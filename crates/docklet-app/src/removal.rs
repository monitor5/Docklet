//! Removing many things at once: what to ask before, the call, and what to
//! say after. Also the prunes.

use docklet_engine::{
    Client, Error, PruneReport, RemoveFailure, RemoveReport, RemoveResult, ResourceKind,
};

use crate::format::format_bytes;
use crate::table::Row;

/// Names listed after a reason before the rest is counted.
const NAMES_SHOWN: usize = 3;

/// What `count` things of `kind` are called: `container` or `containers`.
pub fn noun(kind: ResourceKind, count: usize) -> &'static str {
    match (kind, count == 1) {
        (ResourceKind::Container, true) => "container",
        (ResourceKind::Container, false) => "containers",
        (ResourceKind::Image, true) => "image",
        (ResourceKind::Image, false) => "images",
        (ResourceKind::Volume, true) => "volume",
        (ResourceKind::Volume, false) => "volumes",
        (ResourceKind::Network, true) => "network",
        (ResourceKind::Network, false) => "networks",
    }
}

/// One item of a bulk removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalItem {
    /// What names it to the engine.
    pub id: String,
    /// What names it to the user.
    pub name: String,
    /// A container that runs, which the removal stops.
    pub running: bool,
    /// An image or volume a container is known to use, which the engine
    /// will refuse to remove.
    pub in_use: bool,
}

/// What a bulk removal is about to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalPlan {
    pub kind: ResourceKind,
    pub items: Vec<RemovalItem>,
}

impl RemovalPlan {
    /// A plan for the rows of one table, leaving out those that cannot be
    /// removed at all.
    pub fn new<'a>(kind: ResourceKind, rows: impl IntoIterator<Item = &'a Row>) -> Self {
        let items = rows
            .into_iter()
            .filter(|row| row.flags.removable)
            .map(|row| RemovalItem {
                id: row.id.clone(),
                name: row.name.clone(),
                running: row.flags.running,
                in_use: row.flags.in_use,
            })
            .collect();
        Self { kind, items }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.items.iter().map(|item| item.id.as_str()).collect()
    }

    /// Whether the engine is known to refuse `item`: an image or a volume
    /// that a container uses. A running container is not kept; it is
    /// stopped.
    fn keeps(&self, item: &RemovalItem) -> bool {
        self.kind != ResourceKind::Container && item.in_use
    }

    /// The plan without what the engine is known to refuse: what asking
    /// for the removal really removes, and what to hand to the engine.
    pub fn going(&self) -> Self {
        let going = self.items.iter().filter(|item| !self.keeps(item));
        Self {
            kind: self.kind,
            items: going.cloned().collect(),
        }
    }

    /// How many of the items `going` leaves out.
    pub fn kept(&self) -> usize {
        self.items.iter().filter(|item| self.keeps(item)).count()
    }

    /// The question to ask before removing: "Remove 7 containers? 2 are
    /// running and will be stopped." It counts what will really go; what
    /// was selected but stays is said after it. For a plan of which nothing
    /// can go there is no question: see [`Self::refusal`].
    pub fn confirmation(&self) -> String {
        let selected = self.items.len();
        let kept = self.kept();
        let going: Vec<&RemovalItem> = self.items.iter().filter(|item| !self.keeps(item)).collect();
        let count = going.len();
        let mut text = match going.as_slice() {
            [] if selected == 0 => return "Nothing is selected.".to_string(),
            [] => return "Nothing of what is selected can be removed.".to_string(),
            [only] => format!("Remove {} {}?", noun(self.kind, 1), only.name),
            _ => format!("Remove {count} {}?", noun(self.kind, count)),
        };
        // What the removal does beyond removing, and what it will not do.
        let running = going.iter().filter(|item| item.running).count();
        if self.kind == ResourceKind::Container && running > 0 {
            // "It is", "1 is" or "2 are", for a part of the items.
            let some_are = match (count, running) {
                (1, _) => "It is".to_string(),
                (_, 1) => "1 is".to_string(),
                (_, running) => format!("{running} are"),
            };
            text.push_str(&format!(" {some_are} running and will be stopped."));
        }
        if self.kind == ResourceKind::Volume {
            let whose = if count == 1 { "Its" } else { "Their" };
            text.push_str(&format!(" {whose} data will be deleted."));
        }
        if kept > 0 {
            let are = if kept == 1 { "is" } else { "are" };
            text.push_str(&format!(
                " {kept} of the {selected} selected {are} in use and will be kept."
            ));
        }
        text
    }

    /// What to say instead of asking, as a title and a sentence under it,
    /// when something is selected and none of it can be removed: every item
    /// is in use. `None` when there is something to ask about.
    pub fn refusal(&self) -> Option<(String, String)> {
        let count = self.items.len();
        if count == 0 || self.kept() < count {
            return None;
        }
        Some(match self.items.as_slice() {
            [only] => {
                let noun = noun(self.kind, 1);
                // "Image", "Volume".
                let mut title = noun[..1].to_uppercase();
                title.push_str(&format!("{} {} is in use", &noun[1..], only.name));
                let body = format!(
                    "A container uses it, so Docker will not remove it. Remove that container \
                     first, then the {noun}."
                );
                (title, body)
            }
            _ => {
                let nouns = noun(self.kind, count);
                let title = format!("The {count} selected {nouns} are in use");
                let body = format!(
                    "Containers use them, so Docker will not remove them. Remove those \
                     containers first, then the {nouns}."
                );
                (title, body)
            }
        })
    }

    /// What `id` is called in a sentence.
    pub fn name_of<'a>(&'a self, id: &'a str) -> &'a str {
        let item = self.items.iter().find(|item| item.id == id);
        item.map_or(id, |item| item.name.as_str())
    }
}

/// Removes everything in the plan, a few at a time. A failure does not stop
/// the rest: every item gets a result, in the plan's order. See
/// `docklet_engine::Client::remove_many` for how each kind is removed.
pub async fn remove(client: &Client, plan: &RemovalPlan) -> RemoveReport {
    client.remove_many(plan.kind, &plan.ids()).await
}

/// The report of a removal that could not even be started, with every item
/// failed for the same reason.
pub fn not_removed(plan: &RemovalPlan, error: &str) -> RemoveReport {
    let failed = |item: &RemovalItem| {
        RemoveResult::failed(plan.kind, item.id.as_str(), RemoveFailure::Other, error)
    };
    RemoveReport {
        results: plan.items.iter().map(failed).collect(),
    }
}

/// "is in use" for one item, "are in use" for more.
fn failure_phrase(reason: RemoveFailure, count: usize) -> &'static str {
    match (reason, count == 1) {
        (RemoveFailure::InUse, true) => "is in use",
        (RemoveFailure::InUse, false) => "are in use",
        (RemoveFailure::HasDependents, true) => "has dependent images",
        (RemoveFailure::HasDependents, false) => "have dependent images",
        (RemoveFailure::NotFound, true) => "was not found",
        (RemoveFailure::NotFound, false) => "were not found",
        (RemoveFailure::Refused, _) => "cannot be removed",
        (RemoveFailure::Other, _) => "failed",
    }
}

/// "a, b, c and 2 more".
fn name_list(names: &[&str]) -> String {
    let shown = names[..names.len().min(NAMES_SHOWN)].join(", ");
    match names.len().saturating_sub(NAMES_SHOWN) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// What to say after a removal: "Removed 7 containers.", or with failures
/// "Removed 5. 2 are in use: web, db." The engine's own words are added for
/// failures that have no better explanation.
pub fn removal_summary(plan: &RemovalPlan, report: &RemoveReport) -> String {
    let removed = report.removed();
    if report.failed() == 0 {
        return match removed {
            0 => "Nothing was removed.".to_string(),
            _ => format!("Removed {removed} {}.", noun(plan.kind, removed)),
        };
    }
    let outcome = match removed {
        0 => "Nothing was removed.".to_string(),
        _ => format!("Removed {removed}."),
    };
    format!("{outcome} {}", removal_reasons(plan, report))
}

/// How a removal went, in a few words without a full stop, for a title:
/// "Removed 5 of 7 images", "Nothing was removed". Why is in
/// [`removal_reasons`], which says nothing of this again.
pub fn removal_outcome(plan: &RemovalPlan, report: &RemoveReport) -> String {
    let removed = report.removed();
    let asked = report.results.len();
    match (removed, report.failed()) {
        (0, _) => "Nothing was removed".to_string(),
        (_, 0) => format!("Removed {removed} {}", noun(plan.kind, removed)),
        _ => format!("Removed {removed} of {asked} {}", noun(plan.kind, asked)),
    }
}

/// Why items of a removal were left: "2 are in use: web, db. 1 failed: x.
/// The engine said: ..." Empty when nothing failed.
pub fn removal_reasons(plan: &RemovalPlan, report: &RemoveReport) -> String {
    let mut text = String::new();
    for reason in [
        RemoveFailure::InUse,
        RemoveFailure::HasDependents,
        RemoveFailure::NotFound,
        RemoveFailure::Refused,
        RemoveFailure::Other,
    ] {
        let failed: Vec<&RemoveResult> = report
            .results
            .iter()
            .filter(|result| result.reason() == Some(reason))
            .collect();
        if failed.is_empty() {
            continue;
        }
        let names: Vec<&str> = failed
            .iter()
            .map(|result| plan.name_of(&result.id))
            .collect();
        let phrase = failure_phrase(reason, failed.len());
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&format!(
            "{} {phrase}: {}.",
            failed.len(),
            name_list(&names)
        ));
        let message = failed[0].failure.as_ref().map(|(_, message)| message);
        if reason == RemoveFailure::Other
            && let Some(message) = message.filter(|message| !message.is_empty())
        {
            text.push_str(&format!(" The engine said: {message}"));
        }
    }
    text
}

/// A removal of everything unused of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneTarget {
    /// All stopped containers.
    StoppedContainers,
    /// Images without a tag that no container uses.
    DanglingImages,
    /// Every image no container uses.
    UnusedImages,
    /// Unused volumes that were not given a name.
    AnonymousVolumes,
    /// Every volume no container uses, named ones too.
    UnusedVolumes,
    /// Networks no container uses.
    UnusedNetworks,
}

impl PruneTarget {
    /// The prunes that apply to a table of `kind`, the gentler one first.
    pub fn for_kind(kind: ResourceKind) -> &'static [Self] {
        match kind {
            ResourceKind::Container => &[Self::StoppedContainers],
            ResourceKind::Image => &[Self::DanglingImages, Self::UnusedImages],
            ResourceKind::Volume => &[Self::AnonymousVolumes, Self::UnusedVolumes],
            ResourceKind::Network => &[Self::UnusedNetworks],
        }
    }

    pub fn kind(self) -> ResourceKind {
        match self {
            Self::StoppedContainers => ResourceKind::Container,
            Self::DanglingImages | Self::UnusedImages => ResourceKind::Image,
            Self::AnonymousVolumes | Self::UnusedVolumes => ResourceKind::Volume,
            Self::UnusedNetworks => ResourceKind::Network,
        }
    }

    /// What is removed, in the plural: "stopped containers".
    fn what(self) -> &'static str {
        match self {
            Self::StoppedContainers => "stopped containers",
            Self::DanglingImages => "untagged images",
            Self::UnusedImages => "unused images",
            Self::AnonymousVolumes => "unused anonymous volumes",
            Self::UnusedVolumes => "unused volumes",
            Self::UnusedNetworks => "unused networks",
        }
    }

    /// The text of the button or menu item: "Remove stopped containers".
    pub fn label(self) -> String {
        format!("Remove {}", self.what())
    }

    /// The question to ask before pruning.
    pub fn confirmation(self) -> &'static str {
        match self {
            Self::StoppedContainers => "Remove all stopped containers?",
            Self::DanglingImages => "Remove all untagged images that no container uses?",
            Self::UnusedImages => "Remove every image that no container uses?",
            Self::AnonymousVolumes => {
                "Remove all unused volumes without a name? Their data will be deleted."
            }
            Self::UnusedVolumes => {
                "Remove every volume that no container uses? Their data will be deleted."
            }
            Self::UnusedNetworks => "Remove all networks that no container uses?",
        }
    }
}

/// Runs the prune. See the `prune_*` calls of `docklet_engine::Client`.
pub async fn prune(client: &Client, target: PruneTarget) -> Result<PruneReport, Error> {
    match target {
        PruneTarget::StoppedContainers => client.prune_containers().await,
        PruneTarget::DanglingImages => client.prune_images(false).await,
        PruneTarget::UnusedImages => client.prune_images(true).await,
        PruneTarget::AnonymousVolumes => client.prune_volumes(false).await,
        PruneTarget::UnusedVolumes => client.prune_volumes(true).await,
        PruneTarget::UnusedNetworks => client.prune_networks().await,
    }
}

/// What to say after a prune: "Removed 3 stopped containers, reclaimed
/// 1.2 GB."
pub fn prune_summary(target: PruneTarget, report: &PruneReport) -> String {
    let what = target.what();
    if report.deleted.is_empty() && report.untagged.is_empty() {
        return format!("No {what} to remove.");
    }
    let removed = match target.kind() {
        // An image prune lists layers and untagged names, not images, so
        // there is no honest count.
        ResourceKind::Image => format!("Removed {what}"),
        _ => {
            let count = report.deleted.len();
            let what = if count == 1 {
                what.trim_end_matches('s')
            } else {
                what
            };
            format!("Removed {count} {what}")
        }
    };
    match target.kind() {
        // Networks take no disk space.
        ResourceKind::Network => format!("{removed}."),
        _ => format!(
            "{removed}, reclaimed {}.",
            format_bytes(report.space_reclaimed)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::testing::{container, image, network, volume};
    use crate::table::{containers_table, images_table, networks_table, volumes_table};

    fn container_plan(specs: &[(&str, bool)]) -> RemovalPlan {
        let containers: Vec<_> = specs
            .iter()
            .map(|(name, running)| container(name, None, *running))
            .collect();
        let table = containers_table(&containers, None);
        RemovalPlan::new(ResourceKind::Container, &table.rows)
    }

    fn report(plan: &RemovalPlan, failures: &[(&str, RemoveFailure, &str)]) -> RemoveReport {
        let results = plan.items.iter().map(|item| {
            let failure = failures.iter().find(|(name, ..)| *name == item.name);
            match failure {
                Some((_, reason, message)) => {
                    RemoveResult::failed(plan.kind, item.id.as_str(), *reason, *message)
                }
                None => RemoveResult::removed(plan.kind, item.id.as_str()),
            }
        });
        RemoveReport {
            results: results.collect(),
        }
    }

    #[test]
    fn the_confirmation_counts_and_says_what_is_running() {
        let plan = container_plan(&[
            ("a", true),
            ("b", false),
            ("c", true),
            ("d", false),
            ("e", false),
            ("f", false),
            ("g", false),
        ]);
        assert_eq!(
            plan.confirmation(),
            "Remove 7 containers? 2 are running and will be stopped."
        );
        let one_running = container_plan(&[("a", true), ("b", false)]);
        assert_eq!(
            one_running.confirmation(),
            "Remove 2 containers? 1 is running and will be stopped."
        );
        let none_running = container_plan(&[("a", false), ("b", false)]);
        assert_eq!(none_running.confirmation(), "Remove 2 containers?");
    }

    #[test]
    fn the_confirmation_for_one_item_names_it() {
        assert_eq!(
            container_plan(&[("web", false)]).confirmation(),
            "Remove container web?"
        );
        assert_eq!(
            container_plan(&[("web", true)]).confirmation(),
            "Remove container web? It is running and will be stopped."
        );
    }

    #[test]
    fn the_confirmation_for_nothing_says_so() {
        let plan = container_plan(&[]);
        assert!(plan.is_empty());
        assert_eq!(plan.confirmation(), "Nothing is selected.");
    }

    #[test]
    fn the_confirmation_for_images_says_which_will_be_kept() {
        let mut used = image("sha256:a", &["nginx:latest"], 1, 0);
        used.containers = 1;
        let images = [used, image("sha256:b", &["redis:7"], 1, 0)];
        let table = images_table(&images);
        let plan = RemovalPlan::new(ResourceKind::Image, &table.rows);
        // The question counts what goes, not what was selected.
        assert_eq!(
            plan.confirmation(),
            "Remove image redis:7? 1 of the 2 selected is in use and will be kept."
        );
        assert_eq!(plan.kept(), 1);
        assert_eq!(plan.going().ids(), ["sha256:b"]);
        assert_eq!(plan.refusal(), None);
    }

    /// A plan of `kind` whose items are in use where `in_use` says so.
    fn plan_of(kind: ResourceKind, in_use: &[bool]) -> RemovalPlan {
        let items = in_use
            .iter()
            .enumerate()
            .map(|(index, in_use)| RemovalItem {
                id: format!("id-{index}"),
                name: format!("item-{index}"),
                running: false,
                in_use: *in_use,
            });
        RemovalPlan {
            kind,
            items: items.collect(),
        }
    }

    #[test]
    fn the_question_counts_what_will_really_go() {
        let images = plan_of(ResourceKind::Image, &[true, false, false, true, false]);
        assert_eq!(
            images.confirmation(),
            "Remove 3 images? 2 of the 5 selected are in use and will be kept."
        );
        assert_eq!(images.going().items.len(), 3);
        let volumes = plan_of(ResourceKind::Volume, &[false, true, false]);
        assert_eq!(
            volumes.confirmation(),
            "Remove 2 volumes? Their data will be deleted. 1 of the 3 selected is in use and \
             will be kept."
        );
        // Nothing is known to be refused: the plan goes as it is.
        let free = plan_of(ResourceKind::Image, &[false, false]);
        assert_eq!(free.going(), free);
        assert_eq!(free.kept(), 0);
    }

    #[test]
    fn a_selection_of_which_nothing_can_go_is_refused_not_asked_about() {
        let both = plan_of(ResourceKind::Image, &[true, true]);
        assert!(both.going().is_empty());
        let (title, body) = both.refusal().unwrap();
        assert_eq!(title, "The 2 selected images are in use");
        assert_eq!(
            body,
            "Containers use them, so Docker will not remove them. Remove those containers \
             first, then the images."
        );
        let one = plan_of(ResourceKind::Volume, &[true]);
        let (title, body) = one.refusal().unwrap();
        assert_eq!(title, "Volume item-0 is in use");
        assert_eq!(
            body,
            "A container uses it, so Docker will not remove it. Remove that container first, \
             then the volume."
        );
        // The title and the sentence under it say different things.
        assert!(!body.contains(&title));
        // A running container is stopped, not kept; and nothing selected is
        // nothing to refuse.
        let containers = plan_of(ResourceKind::Container, &[true, true]);
        assert_eq!(containers.refusal(), None);
        assert_eq!(containers.kept(), 0);
        assert_eq!(plan_of(ResourceKind::Image, &[]).refusal(), None);
    }

    #[test]
    fn the_outcome_and_the_reasons_do_not_repeat_each_other() {
        let plan = container_plan(&[("a", false), ("b", false), ("c", false)]);
        let in_use = RemoveFailure::InUse;
        let some = report(&plan, &[("b", in_use, "x")]);
        assert_eq!(removal_outcome(&plan, &some), "Removed 2 of 3 containers");
        assert_eq!(removal_reasons(&plan, &some), "1 is in use: b.");
        let none = report(
            &plan,
            &[("a", in_use, "x"), ("b", in_use, "x"), ("c", in_use, "x")],
        );
        assert_eq!(removal_outcome(&plan, &none), "Nothing was removed");
        assert_eq!(removal_reasons(&plan, &none), "3 are in use: a, b, c.");
        let all = report(&plan, &[]);
        assert_eq!(removal_outcome(&plan, &all), "Removed 3 containers");
        assert_eq!(removal_reasons(&plan, &all), "");
    }

    #[test]
    fn the_confirmation_for_volumes_warns_about_their_data() {
        let table = volumes_table(&[volume("data"), volume("cache")], None);
        let plan = RemovalPlan::new(ResourceKind::Volume, &table.rows);
        assert_eq!(
            plan.confirmation(),
            "Remove 2 volumes? Their data will be deleted."
        );
        let one = RemovalPlan::new(ResourceKind::Volume, &table.rows[..1]);
        assert_eq!(
            one.confirmation(),
            "Remove volume data? Its data will be deleted."
        );
    }

    #[test]
    fn a_plan_leaves_out_what_cannot_be_removed() {
        let table = networks_table(&[network("bridge"), network("shop_default")]);
        let plan = RemovalPlan::new(ResourceKind::Network, &table.rows);
        assert_eq!(plan.ids(), ["net-shop_default"]);
        assert_eq!(plan.confirmation(), "Remove network shop_default?");
    }

    #[test]
    fn a_removal_that_worked_says_how_many() {
        let plan = container_plan(&[("a", false), ("b", true), ("c", false)]);
        assert_eq!(
            removal_summary(&plan, &report(&plan, &[])),
            "Removed 3 containers."
        );
        let one = container_plan(&[("a", false)]);
        assert_eq!(
            removal_summary(&one, &report(&one, &[])),
            "Removed 1 container."
        );
    }

    #[test]
    fn failures_are_counted_by_reason_and_named() {
        let images: Vec<_> = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .map(|name| {
                let tag = format!("{name}:1");
                image(&format!("sha256:{name}"), &[tag.as_str()], 1, 0)
            })
            .collect();
        let table = images_table(&images);
        let plan = RemovalPlan::new(ResourceKind::Image, &table.rows);
        let in_use = RemoveFailure::InUse;
        let outcome = report(&plan, &[("c:1", in_use, "x"), ("f:1", in_use, "x")]);
        assert_eq!(
            removal_summary(&plan, &outcome),
            "Removed 5. 2 are in use: c:1, f:1."
        );
    }

    #[test]
    fn each_reason_gets_its_own_sentence_in_a_fixed_order() {
        let plan = container_plan(&[("a", false), ("b", false), ("c", false), ("d", false)]);
        let outcome = report(
            &plan,
            &[
                ("d", RemoveFailure::NotFound, "no such container"),
                ("b", RemoveFailure::InUse, "in use"),
                ("c", RemoveFailure::Refused, "no"),
            ],
        );
        assert_eq!(
            removal_summary(&plan, &outcome),
            "Removed 1. 1 is in use: b. 1 was not found: d. 1 cannot be removed: c."
        );
    }

    #[test]
    fn long_lists_of_names_are_cut() {
        let plan = container_plan(&[
            ("a", false),
            ("b", false),
            ("c", false),
            ("d", false),
            ("e", false),
        ]);
        let all: Vec<_> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| (*name, RemoveFailure::HasDependents, "x"))
            .collect();
        assert_eq!(
            removal_summary(&plan, &report(&plan, &all)),
            "Nothing was removed. 5 have dependent images: a, b, c and 2 more."
        );
    }

    #[test]
    fn an_unexplained_failure_comes_with_the_engines_words() {
        let plan = container_plan(&[("a", false), ("b", false)]);
        let outcome = report(&plan, &[("b", RemoveFailure::Other, "driver failed")]);
        assert_eq!(
            removal_summary(&plan, &outcome),
            "Removed 1. 1 failed: b. The engine said: driver failed"
        );
    }

    #[test]
    fn a_removal_that_could_not_start_fails_every_item() {
        let plan = container_plan(&[("a", false), ("b", false)]);
        let outcome = not_removed(&plan, "the engine is not running");
        assert_eq!(outcome.failed(), 2);
        assert_eq!(outcome.count(RemoveFailure::Other), 2);
        assert_eq!(
            removal_summary(&plan, &outcome),
            "Nothing was removed. 2 failed: a, b. The engine said: the engine is not running"
        );
    }

    #[test]
    fn an_empty_report_says_nothing_was_removed() {
        let plan = container_plan(&[]);
        assert_eq!(
            removal_summary(&plan, &RemoveReport::default()),
            "Nothing was removed."
        );
    }

    fn pruned(deleted: &[&str], untagged: &[&str], space_reclaimed: u64) -> PruneReport {
        PruneReport {
            deleted: deleted.iter().map(|id| id.to_string()).collect(),
            untagged: untagged.iter().map(|name| name.to_string()).collect(),
            space_reclaimed,
        }
    }

    #[test]
    fn a_prune_says_what_it_removed_and_how_much_it_freed() {
        assert_eq!(
            prune_summary(
                PruneTarget::StoppedContainers,
                &pruned(&["a", "b", "c"], &[], 1_200_000_000)
            ),
            "Removed 3 stopped containers, reclaimed 1.2 GB."
        );
        assert_eq!(
            prune_summary(PruneTarget::UnusedVolumes, &pruned(&["data"], &[], 500)),
            "Removed 1 unused volume, reclaimed 500 B."
        );
        // Layers and untagged names are not images: no count.
        assert_eq!(
            prune_summary(
                PruneTarget::UnusedImages,
                &pruned(&["sha256:a", "sha256:b"], &["nginx:latest"], 412_000_000)
            ),
            "Removed unused images, reclaimed 412 MB."
        );
        assert_eq!(
            prune_summary(PruneTarget::UnusedNetworks, &pruned(&["a", "b"], &[], 0)),
            "Removed 2 unused networks."
        );
    }

    #[test]
    fn a_prune_that_found_nothing_says_so() {
        for (target, text) in [
            (
                PruneTarget::StoppedContainers,
                "No stopped containers to remove.",
            ),
            (PruneTarget::DanglingImages, "No untagged images to remove."),
            (
                PruneTarget::AnonymousVolumes,
                "No unused anonymous volumes to remove.",
            ),
            (PruneTarget::UnusedNetworks, "No unused networks to remove."),
        ] {
            assert_eq!(prune_summary(target, &PruneReport::default()), text);
        }
    }

    #[test]
    fn an_image_prune_that_only_untagged_still_did_something() {
        let report = pruned(&[], &["old:1"], 0);
        assert_eq!(
            prune_summary(PruneTarget::DanglingImages, &report),
            "Removed untagged images, reclaimed 0 B."
        );
    }

    #[test]
    fn every_prune_belongs_to_the_table_of_its_kind() {
        for kind in ResourceKind::REMOVAL_ORDER {
            let targets = PruneTarget::for_kind(kind);
            assert!(!targets.is_empty());
            for target in targets {
                assert_eq!(target.kind(), kind);
                assert!(target.label().starts_with("Remove "));
                assert!(target.confirmation().ends_with('?') || kind == ResourceKind::Volume);
            }
        }
        assert_eq!(
            PruneTarget::StoppedContainers.label(),
            "Remove stopped containers"
        );
    }
}
