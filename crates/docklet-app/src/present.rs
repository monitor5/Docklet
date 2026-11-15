//! The models in the plain form a window draws (`view`). All of it is pure.

use docklet_core::EngineState;
use docklet_engine::{DiskTotals, DiskUsage, RemoveReport, ResourceKind};

use crate::format::{format_bytes, format_count};
use crate::list::{Item, ListModel};
use crate::pull::pull_button_label;
use crate::removal::{PruneTarget, RemovalPlan, noun, removal_summary};
use crate::screen::{EngineCommand, WSL_INSTALL_COMMAND, screen_text, state_label};
use crate::table::{RowAction, SortDirection, container_column};
use crate::usage::{Meter, MeterState};
use crate::view;

/// The lists in the order of their tabs.
pub const TABS: [ResourceKind; 4] = [
    ResourceKind::Container,
    ResourceKind::Image,
    ResourceKind::Volume,
    ResourceKind::Network,
];

/// The names things are asked for by, in `view::Action` and `view::Button`.
pub mod key {
    pub const START: &str = "start";
    pub const STOP: &str = "stop";
    pub const RESTART: &str = "restart";
    pub const LOGS: &str = "logs";
    pub const REMOVE: &str = "remove";
    /// Opens the first port a container publishes.
    pub const OPEN: &str = "open";
    /// Followed by the port: `open:8080`.
    pub const OPEN_PORT: &str = "open:";

    pub const ENGINE_START: &str = "engine-start";
    pub const ENGINE_STOP: &str = "engine-stop";
    pub const REFRESH: &str = "refresh";
    pub const INSTALL: &str = "install";
    pub const COPY_COMMAND: &str = "copy-command";

    /// Followed by the index into `PruneTarget::for_kind`: `prune:1`.
    pub const PRUNE: &str = "prune:";
    pub const DISK_USAGE: &str = "disk-usage";
}

pub fn tab_titles() -> Vec<String> {
    ["Containers", "Images", "Volumes", "Networks"]
        .map(str::to_string)
        .to_vec()
}

pub fn tab_of(kind: ResourceKind) -> usize {
    TABS.iter().position(|tab| *tab == kind).unwrap_or(0)
}

pub fn kind_of(tab: usize) -> Option<ResourceKind> {
    TABS.get(tab).copied()
}

/// What can be done to a row of a list of `kind`, in the order of the
/// buttons. A row's `view::Row::actions` has a bit for each. The port in
/// `OpenPort` stands for "a port": the button opens the row's first one,
/// and each port has an item of its own in the row's menu.
fn row_actions(kind: ResourceKind) -> Vec<(RowAction, view::Action)> {
    let remove = (RowAction::Remove, view::Action::new(key::REMOVE, "Remove"));
    match kind {
        ResourceKind::Container => vec![
            (RowAction::Start, view::Action::new(key::START, "Start")),
            (RowAction::Stop, view::Action::new(key::STOP, "Stop")),
            (
                RowAction::Restart,
                view::Action::in_menu(key::RESTART, "Restart"),
            ),
            (RowAction::Logs, view::Action::new(key::LOGS, "Logs")),
            (RowAction::OpenPort(0), view::Action::new(key::OPEN, "Open")),
            remove,
        ],
        _ => vec![remove],
    }
}

/// Whether `action` of `row_actions` applies to `row` as it is now.
fn applies(row: &crate::table::Row, action: RowAction) -> bool {
    match action {
        RowAction::OpenPort(_) => !row.ports.is_empty(),
        action => row.actions.contains(&action),
    }
}

/// Narrowest each column gets, in device-independent pixels.
fn min_widths(kind: ResourceKind) -> &'static [u16] {
    match kind {
        ResourceKind::Container => &[130, 90, 80, 60, 56, 70, 110, 110],
        ResourceKind::Image => &[160, 100, 70, 90],
        ResourceKind::Volume | ResourceKind::Network => &[160, 70, 70],
    }
}

fn columns(list: &ListModel) -> Vec<view::Column> {
    let kind = list.kind();
    let widths = min_widths(kind);
    list.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| view::Column {
            title: column.title.to_string(),
            align: match column.align {
                crate::table::Align::Left => view::Align::Left,
                crate::table::Align::Right => view::Align::Right,
            },
            weight: column.width,
            min_width: widths.get(index).copied().unwrap_or(60),
            sortable: true,
            status: kind == ResourceKind::Container && index == container_column::STATE,
        })
        .collect()
}

/// A list as it shows right now. `loaded` is false until the engine has
/// answered for it once; `disk` adds what the list takes up to its summary.
pub fn table_view(list: &ListModel, loaded: bool, disk: Option<&DiskUsage>) -> view::Table {
    let actions = row_actions(list.kind());
    let lines = (0..list.len())
        .filter_map(|index| list.item(index))
        .map(|item| match item {
            Item::Group {
                name,
                count,
                collapsed,
            } => view::Line::Group {
                name: name.to_string(),
                count,
                collapsed,
                check: (!collapsed).then(|| list.group_coverage(name) == view::Coverage::All),
            },
            Item::Row(row) => {
                let mut flags = view::RowFlags::default();
                if row.flags.running {
                    flags = flags | view::RowFlags::ACTIVE;
                }
                if list.is_busy(&row.id) {
                    flags = flags | view::RowFlags::BUSY;
                }
                let mask = actions
                    .iter()
                    .enumerate()
                    .filter(|(_, (action, _))| applies(row, *action))
                    .fold(0u32, |mask, (index, _)| mask | 1 << index);
                let extra = row
                    .ports
                    .iter()
                    .map(|port| {
                        let label = format!("Open localhost:{port}");
                        view::Action::in_menu(format!("{}{port}", key::OPEN_PORT), label)
                    })
                    .collect();
                view::Line::Row(view::Row {
                    id: row.id.clone(),
                    cells: row.cells.clone(),
                    check: row.flags.removable.then(|| list.is_selected(&row.id)),
                    grouped: row.group.is_some(),
                    flags,
                    actions: mask,
                    extra,
                })
            }
        })
        .collect();
    let default_action = actions
        .iter()
        .position(|(action, _)| *action == RowAction::Logs);
    view::Table {
        columns: columns(list),
        actions: actions.into_iter().map(|(_, action)| action).collect(),
        default_action,
        lines,
        sort: list.sort().map(|sort| view::SortMark {
            column: sort.column,
            ascending: sort.direction == SortDirection::Ascending,
        }),
        select_all: list.coverage(),
        selected: list.selected_count(),
        summary: summary(list, loaded, disk),
        commands: commands(list.kind()),
    }
}

/// The line next to the filter: how many rows there are, show or are
/// selected, or why there are none.
pub fn summary(list: &ListModel, loaded: bool, disk: Option<&DiskUsage>) -> String {
    if !loaded {
        return "Loading…".to_string();
    }
    let selected = list.selected_count();
    if selected > 0 {
        return format!("{} selected", format_count(selected));
    }
    if list.is_empty() {
        return list.empty_text().to_string();
    }
    let (matching, total) = (list.matching(), list.total());
    let noun = noun(list.kind(), total);
    let mut text = if matching == total {
        format!("{} {noun}", format_count(total))
    } else {
        format!(
            "{} of {} {noun}",
            format_count(matching),
            format_count(total)
        )
    };
    let totals = disk.and_then(|disk| match list.kind() {
        ResourceKind::Image => Some(disk.image_totals),
        ResourceKind::Volume => Some(disk.volume_totals),
        _ => None,
    });
    if let Some(totals) = totals {
        text.push_str(&format!(
            " · {}, {} reclaimable",
            format_bytes(totals.size),
            format_bytes(totals.reclaimable)
        ));
    }
    text
}

/// The menu of a list of `kind`: its prunes, the gentler one first, and for
/// what takes up disk space the request to measure it.
pub fn commands(kind: ResourceKind) -> Vec<view::Action> {
    let mut commands: Vec<view::Action> = PruneTarget::for_kind(kind)
        .iter()
        .enumerate()
        .map(|(index, target)| {
            view::Action::in_menu(format!("{}{index}", key::PRUNE), target.label())
        })
        .collect();
    if matches!(kind, ResourceKind::Image | ResourceKind::Volume) {
        let label = "Measure disk usage";
        commands.push(view::Action::in_menu(key::DISK_USAGE, label));
    }
    commands
}

/// The prune a command of `commands(kind)` stands for.
pub fn prune_of(kind: ResourceKind, command: &str) -> Option<PruneTarget> {
    let index: usize = command.strip_prefix(key::PRUNE)?.parse().ok()?;
    PruneTarget::for_kind(kind).get(index).copied()
}

/// The port a row action of `view::Row::extra` opens.
pub fn port_of(action: &str) -> Option<u16> {
    action.strip_prefix(key::OPEN_PORT)?.parse().ok()
}

/// The usage strip. A meter without a figure says so in a word; why is in
/// its `detail`.
pub fn meters_view(meters: &[Meter]) -> Vec<view::Meter> {
    meters
        .iter()
        .map(|meter| {
            let text = match meter.state {
                MeterState::Known | MeterState::Off => meter.value.as_str(),
                MeterState::Measuring => "Measuring…",
                MeterState::Unknown => "Unknown",
                MeterState::NotMeasured => "Not measured",
            };
            view::Meter {
                label: meter.label.to_string(),
                text: text.to_string(),
                fill: meter.fraction.map(|fraction| fraction as f32),
                known: meter.state == MeterState::Known,
                detail: meter.detail.clone(),
            }
        })
        .collect()
}

/// What a click on the usage strip shows: every meter with its sentence.
pub fn usage_notice(meters: &[Meter]) -> view::Notice {
    let lines: Vec<String> = meters_view(meters)
        .iter()
        .map(|meter| format!("{}: {}. {}", meter.label, meter.text, meter.detail))
        .collect();
    view::Notice {
        title: "What the engine uses".to_string(),
        message: lines.join("\n\n"),
        detail: None,
        tone: view::Tone::Info,
    }
}

/// Docker's disk usage in words, kind by kind.
pub fn disk_notice(usage: &DiskUsage) -> view::Notice {
    let line = |what: &str, totals: DiskTotals| {
        format!(
            "{what}: {} in {}, {} in use, {} reclaimable",
            format_bytes(totals.size),
            format_count(totals.count as usize),
            format_count(totals.active as usize),
            format_bytes(totals.reclaimable)
        )
    };
    let lines = [
        line("Images", usage.image_totals),
        line("Containers", usage.container_totals),
        line("Volumes", usage.volume_totals),
        line("Build cache", usage.build_cache_totals),
    ];
    view::Notice {
        title: format!("Docker keeps {} on disk", format_bytes(usage.total_size())),
        message: format!(
            "{} of it can be reclaimed by removing what no container uses.",
            format_bytes(usage.total_reclaimable())
        ),
        detail: Some(lines.join("\n")),
        tone: view::Tone::Info,
    }
}

fn button_key(command: EngineCommand) -> &'static str {
    match command {
        EngineCommand::Install => key::INSTALL,
        EngineCommand::Start => key::ENGINE_START,
        EngineCommand::Stop => key::ENGINE_STOP,
        EngineCommand::Refresh => key::REFRESH,
    }
}

/// The message screen for `state`. `acting` disables Start and Stop: one of
/// the window's own engine requests is in flight.
pub fn engine_screen(state: &EngineState, acting: bool) -> view::Screen {
    let text = screen_text(state);
    let mut buttons = text.buttons.iter().map(|button| {
        let waits = matches!(button.command, EngineCommand::Start | EngineCommand::Stop);
        view::Button::new(button_key(button.command), button.label).enabled(!(acting && waits))
    });
    let primary = buttons.next();
    let secondary = match text.command {
        // The command is also in the detail box, to be read and selected.
        Some(_) => Some(view::Button::new(key::COPY_COMMAND, "Copy command")),
        None => buttons.next(),
    };
    view::Screen {
        title: text.title.to_string(),
        body: text.body.unwrap_or_default().to_string(),
        detail: text.detail.or(text.command.map(str::to_string)),
        progress: if text.progress {
            view::Progress::Busy
        } else {
            view::Progress::None
        },
        choice: None,
        primary,
        secondary,
    }
}

/// The command the WSL screen offers to copy.
pub fn copied_command() -> &'static str {
    WSL_INSTALL_COMMAND
}

/// The install screen: the base to pick, and the steps of a running
/// install.
pub fn install_screen(
    bases: &[&str],
    selected: usize,
    installing: bool,
    steps: &[String],
) -> view::Screen {
    let mut screen = engine_screen(&EngineState::NotInstalled, false);
    screen.choice = Some(view::Choice {
        label: "Base".to_string(),
        options: bases.iter().map(|base| base.to_string()).collect(),
        selected: selected.min(bases.len().saturating_sub(1)),
        enabled: !installing && !bases.is_empty(),
    });
    if !steps.is_empty() {
        screen.detail = Some(steps.join("\n"));
    }
    if installing {
        screen.progress = view::Progress::Busy;
        screen.primary = Some(view::Button::new(key::INSTALL, "Installing…").enabled(false));
    }
    screen
}

/// What shows until the first answer about the engine has arrived.
pub fn connecting_screen() -> view::Screen {
    view::Screen {
        title: "Connecting…".to_string(),
        progress: view::Progress::Busy,
        ..view::Screen::default()
    }
}

/// The right end of the top row. `state` is `None` until the first answer.
pub fn header_view(state: Option<&EngineState>, acting: bool) -> view::Header {
    view::Header {
        status: state.map_or("Connecting…", state_label).to_string(),
        button: (state == Some(&EngineState::Running))
            .then(|| view::Button::new(key::ENGINE_STOP, "Stop engine").enabled(!acting)),
    }
}

/// The tray's tooltip and what its menu offers. `state` is `None` while
/// nobody looks at the engine (the window is away): then both are offered,
/// and the engine turns down what does not apply.
pub fn tray_view(state: Option<&EngineState>, acting: bool) -> view::TrayState {
    let offers = |wanted: EngineCommand| match state {
        Some(state) => {
            let buttons = screen_text(state).buttons;
            buttons.iter().any(|button| button.command == wanted)
        }
        None => true,
    };
    view::TrayState {
        tooltip: match state {
            Some(state) => format!("Docklet: {}", state_label(state)),
            None => "Docklet".to_string(),
        },
        can_start: !acting && offers(EngineCommand::Start),
        can_stop: !acting && offers(EngineCommand::Stop),
    }
}

/// Splits `Remove 3 containers? 1 is running and will be stopped.` into the
/// question and what follows it.
pub fn question(text: &str) -> (String, String) {
    match text.find('?') {
        Some(end) => (text[..=end].to_string(), text[end + 1..].trim().to_string()),
        None => (text.to_string(), String::new()),
    }
}

/// The question to ask before a removal.
pub fn removal_confirm(token: u64, plan: &RemovalPlan) -> view::Confirm {
    let (title, rest) = question(&plan.confirmation());
    let message = if rest.is_empty() {
        "This cannot be undone.".to_string()
    } else {
        format!("{rest} This cannot be undone.")
    };
    view::Confirm {
        token,
        title,
        message,
        accept: match plan.items.len() {
            1 => "Remove".to_string(),
            count => format!("Remove {count}"),
        },
    }
}

/// The question to ask before a prune.
pub fn prune_confirm(token: u64, target: PruneTarget) -> view::Confirm {
    let (title, rest) = question(target.confirmation());
    let message = if rest.is_empty() {
        "This cannot be undone.".to_string()
    } else {
        format!("{rest} This cannot be undone.")
    };
    view::Confirm {
        token,
        title,
        message,
        accept: "Remove".to_string(),
    }
}

/// What to show after a removal that left something behind: the summary,
/// and what the engine said about each item.
pub fn removal_notice(plan: &RemovalPlan, report: &RemoveReport) -> view::Notice {
    let failures: Vec<String> = report
        .results
        .iter()
        .filter_map(|result| {
            let (_, message) = result.failure.as_ref()?;
            Some(format!("{}: {message}", plan.name_of(&result.id)))
        })
        .collect();
    view::Notice {
        title: if report.removed() == 0 {
            "Nothing was removed".to_string()
        } else {
            "Not everything was removed".to_string()
        },
        message: removal_summary(plan, report),
        detail: (!failures.is_empty()).then(|| failures.join("\n")),
        tone: view::Tone::Warning,
    }
}

/// The line above the images table where an image to pull is typed.
pub fn pull_entry(pulling: bool, status: &str) -> view::Entry {
    view::Entry {
        label: "Pull".to_string(),
        cue: "nginx:latest".to_string(),
        button: pull_button_label(pulling).to_string(),
        enabled: !pulling,
        status: status.to_string(),
        text: None,
    }
}

#[cfg(test)]
mod tests {
    use docklet_engine::{RemoveFailure, RemoveResult};

    use super::*;
    use crate::table::testing::{container, image, network, usage, volume};
    use crate::table::{containers_table, images_table, networks_table, volumes_table};
    use crate::usage::UsageModel;

    fn containers() -> ListModel {
        let mut web = container("web", Some("shop"), true);
        web.ports = Some(vec![docklet_engine::models::PortSummary {
            ip: None,
            private_port: 80,
            public_port: Some(8080),
            typ: None,
        }]);
        let mut list = ListModel::new(ResourceKind::Container);
        list.set_table(containers_table(
            &[
                web,
                container("db", Some("shop"), false),
                container("solo", None, true),
            ],
            None,
        ));
        list
    }

    fn rows(table: &view::Table) -> Vec<&view::Row> {
        table
            .lines
            .iter()
            .filter_map(|line| match line {
                view::Line::Row(row) => Some(row),
                view::Line::Group { .. } => None,
            })
            .collect()
    }

    #[test]
    fn the_tabs_are_the_four_lists() {
        assert_eq!(tab_titles().len(), TABS.len());
        for (tab, kind) in TABS.into_iter().enumerate() {
            assert_eq!(tab_of(kind), tab);
            assert_eq!(kind_of(tab), Some(kind));
        }
        assert_eq!(kind_of(4), None);
    }

    #[test]
    fn a_list_becomes_lines_with_group_headers() {
        let list = containers();
        let table = table_view(&list, true, None);
        assert_eq!(table.columns.len(), 8);
        assert!(table.columns[container_column::STATE].status);
        assert_eq!(
            table.columns[container_column::CPU].align,
            view::Align::Right
        );
        assert!(matches!(
            &table.lines[0],
            view::Line::Group { name, count: 2, collapsed: false, check: Some(false) } if name == "shop"
        ));
        let rows = rows(&table);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].cells[0], "db");
        assert!(rows[0].grouped && !rows[2].grouped);
        assert_eq!(table.summary, "3 containers");
        assert_eq!(table.select_all, view::Coverage::None);
        assert_eq!(table.sort, None);
    }

    #[test]
    fn a_rows_buttons_follow_from_what_applies_to_it() {
        let list = containers();
        let table = table_view(&list, true, None);
        let keys: Vec<&str> = table
            .actions
            .iter()
            .map(|action| action.key.as_str())
            .collect();
        assert_eq!(keys, ["start", "stop", "restart", "logs", "open", "remove"]);
        // Restart is in the context menu only.
        assert!(!table.actions[2].button);
        assert_eq!(table.default_action, Some(3));
        let rows = rows(&table);
        let enabled = |row: &view::Row| -> Vec<bool> {
            (0..6).map(|action| row.action_enabled(action)).collect()
        };
        // db is stopped and publishes nothing; web runs and has a port.
        assert_eq!(enabled(rows[0]), [true, false, false, true, false, true]);
        assert_eq!(enabled(rows[1]), [false, true, true, true, true, true]);
        assert!(rows[1].flags.contains(view::RowFlags::ACTIVE));
        assert!(!rows[0].flags.contains(view::RowFlags::ACTIVE));
    }

    #[test]
    fn a_published_port_is_an_action_of_its_row() {
        let table = table_view(&containers(), true, None);
        let web = rows(&table)[1];
        assert_eq!(web.extra.len(), 1);
        assert_eq!(web.extra[0].key, "open:8080");
        assert_eq!(web.extra[0].label, "Open localhost:8080");
        assert_eq!(port_of(&web.extra[0].key), Some(8080));
        assert_eq!(port_of("open:99999"), None);
        assert_eq!(port_of("logs"), None);
    }

    #[test]
    fn a_busy_row_has_no_actions() {
        let mut list = containers();
        list.set_busy("id-solo", true);
        let table = table_view(&list, true, None);
        let solo = rows(&table)[2];
        assert!(solo.flags.contains(view::RowFlags::BUSY));
        assert!(!solo.action_enabled(1));
    }

    #[test]
    fn the_selection_shows_in_the_boxes_and_the_summary() {
        let mut list = containers();
        list.toggle("id-db");
        let table = table_view(&list, true, None);
        assert_eq!(rows(&table)[0].check, Some(true));
        assert_eq!(rows(&table)[1].check, Some(false));
        assert_eq!(table.selected, 1);
        assert_eq!(table.select_all, view::Coverage::Some);
        assert_eq!(table.summary, "1 selected");

        list.toggle_group_selection("shop");
        let table = table_view(&list, true, None);
        assert!(matches!(
            table.lines[0],
            view::Line::Group {
                check: Some(true),
                ..
            }
        ));
        // A collapsed group has no box: its rows cannot be selected.
        list.toggle_group("shop");
        let table = table_view(&list, true, None);
        assert!(matches!(
            table.lines[0],
            view::Line::Group {
                collapsed: true,
                check: None,
                ..
            }
        ));
        assert_eq!(table.selected, 0);
    }

    #[test]
    fn what_cannot_be_removed_has_no_box_and_no_remove() {
        let mut list = ListModel::new(ResourceKind::Network);
        list.set_table(networks_table(&[network("bridge"), network("app")]));
        let table = table_view(&list, true, None);
        let rows = rows(&table);
        assert_eq!(rows[0].check, None);
        assert!(!rows[0].action_enabled(0));
        assert_eq!(rows[1].check, Some(false));
        assert!(rows[1].action_enabled(0));
        assert_eq!(table.default_action, None);
    }

    #[test]
    fn the_summary_says_why_a_list_shows_nothing() {
        let mut list = ListModel::new(ResourceKind::Image);
        assert_eq!(summary(&list, false, None), "Loading…");
        assert_eq!(summary(&list, true, None), "No images.");
        list.set_table(images_table(&[
            image("sha256:a", &["nginx:1"], 1, 0),
            image("sha256:b", &["redis:8"], 1, 0),
        ]));
        assert_eq!(summary(&list, true, None), "2 images");
        list.set_filter("nginx");
        assert_eq!(summary(&list, true, None), "1 of 2 images");
        list.set_filter("nothing");
        assert_eq!(summary(&list, true, None), "Nothing matches the filter.");
    }

    #[test]
    fn disk_usage_is_added_to_the_lists_it_is_about() {
        let disk = DiskUsage {
            image_totals: DiskTotals {
                count: 2,
                active: 1,
                size: 6_000_000_000,
                reclaimable: 2_000_000_000,
            },
            ..Default::default()
        };
        let mut images = ListModel::new(ResourceKind::Image);
        images.set_table(images_table(&[image("sha256:a", &["nginx:1"], 1, 0)]));
        assert_eq!(
            summary(&images, true, Some(&disk)),
            "1 image · 6.0 GB, 2.0 GB reclaimable"
        );
        let mut volumes = ListModel::new(ResourceKind::Volume);
        volumes.set_table(volumes_table(&[volume("data")], Some(&disk)));
        assert_eq!(
            summary(&volumes, true, Some(&disk)),
            "1 volume · 0 B, 0 B reclaimable"
        );
        // Containers say nothing about the disk.
        assert_eq!(summary(&containers(), true, Some(&disk)), "3 containers");
    }

    #[test]
    fn every_list_offers_its_prunes_and_two_of_them_the_disk_usage() {
        let labels = |kind| -> Vec<String> {
            commands(kind)
                .into_iter()
                .map(|command| command.label)
                .collect()
        };
        assert_eq!(
            labels(ResourceKind::Container),
            ["Remove stopped containers"]
        );
        assert_eq!(
            labels(ResourceKind::Image),
            [
                "Remove untagged images",
                "Remove unused images",
                "Measure disk usage"
            ]
        );
        assert_eq!(labels(ResourceKind::Network), ["Remove unused networks"]);
        assert_eq!(
            prune_of(ResourceKind::Volume, "prune:1"),
            Some(PruneTarget::UnusedVolumes)
        );
        assert_eq!(prune_of(ResourceKind::Volume, "prune:2"), None);
        assert_eq!(prune_of(ResourceKind::Volume, "disk-usage"), None);
    }

    #[test]
    fn a_meter_without_a_figure_says_so_in_a_word() {
        let mut model = UsageModel::default();
        let words = |model: &UsageModel| -> Vec<String> {
            meters_view(&model.meters())
                .into_iter()
                .map(|meter| meter.text)
                .collect()
        };
        assert_eq!(
            words(&model),
            [
                "Measuring…",
                "Measuring…",
                "Measuring…",
                "Measuring…",
                "Not measured"
            ]
        );
        model.set_host(crate::streams::HostReading::Failed("no access".to_string()));
        let meters = meters_view(&model.meters());
        assert!(meters[..4].iter().all(|meter| meter.text == "Unknown"));
        assert!(meters.iter().all(|meter| !meter.known));
        assert_eq!(meters[0].detail, "no access");
        assert_eq!(meters[0].fill, None);
        let notice = usage_notice(&model.meters());
        assert!(notice.message.contains("Memory: Unknown. no access"));
    }

    #[test]
    fn usage_columns_arrive_in_the_rows() {
        let mut list = containers();
        list.set_usage(Some(&[usage("web", Some(12.5), 52_400_000)]));
        let table = table_view(&list, true, None);
        assert_eq!(
            rows(&table)[1].cells[container_column::CPU..],
            ["12.5%", "52.4 MB", "1.2 MB / 340 KB", "4.1 MB / 12.0 KB"]
        );
        // Running without a sample yet: text, not an error.
        assert_eq!(rows(&table)[2].cells[container_column::CPU], "—");
    }

    #[test]
    fn each_engine_state_has_its_screen_and_buttons() {
        let keys = |screen: &view::Screen| -> Vec<String> {
            [&screen.primary, &screen.secondary]
                .into_iter()
                .flatten()
                .map(|button| button.key.clone())
                .collect()
        };
        let wsl = engine_screen(&EngineState::WslMissing, false);
        assert_eq!(wsl.detail.as_deref(), Some(WSL_INSTALL_COMMAND));
        assert_eq!(keys(&wsl), ["refresh", "copy-command"]);
        assert_eq!(copied_command(), WSL_INSTALL_COMMAND);

        let sleeping = engine_screen(&EngineState::Sleeping, false);
        assert_eq!(keys(&sleeping), ["engine-start", "engine-stop"]);
        assert_eq!(sleeping.progress, view::Progress::None);

        let starting = engine_screen(&EngineState::Starting, true);
        assert_eq!(starting.progress, view::Progress::Busy);
        assert!(!starting.primary.unwrap().enabled);
        assert!(!starting.secondary.unwrap().enabled);

        let error = engine_screen(&EngineState::Error("boom".to_string()), true);
        assert_eq!(error.detail.as_deref(), Some("boom"));
        // Retry only looks; it is never held back.
        assert!(error.primary.unwrap().enabled);
    }

    #[test]
    fn the_install_screen_offers_the_bases_and_shows_the_steps() {
        let bases = ["debian-13", "alpine"];
        let idle = install_screen(&bases, 1, false, &[]);
        let choice = idle.choice.unwrap();
        assert_eq!(choice.options, bases);
        assert_eq!(choice.selected, 1);
        assert!(choice.enabled);
        assert_eq!(idle.primary.unwrap().label, "Install");
        assert_eq!(idle.detail, None);

        let steps = ["Downloading".to_string(), "Importing".to_string()];
        let running = install_screen(&bases, 7, true, &steps);
        assert_eq!(running.progress, view::Progress::Busy);
        assert_eq!(running.detail.as_deref(), Some("Downloading\nImporting"));
        let choice = running.choice.unwrap();
        assert!(!choice.enabled);
        assert_eq!(choice.selected, 1);
        let button = running.primary.unwrap();
        assert_eq!(
            (button.label.as_str(), button.enabled),
            ("Installing…", false)
        );
    }

    #[test]
    fn the_header_and_the_tray_follow_the_engine() {
        assert_eq!(header_view(None, false).status, "Connecting…");
        let running = header_view(Some(&EngineState::Running), true);
        assert_eq!(running.status, "Running");
        assert!(!running.button.unwrap().enabled);
        assert_eq!(header_view(Some(&EngineState::Stopped), false).button, None);

        let tray = tray_view(Some(&EngineState::Running), false);
        assert_eq!(tray.tooltip, "Docklet: Running");
        assert!(tray.can_stop && !tray.can_start);
        let asleep = tray_view(Some(&EngineState::Sleeping), false);
        assert!(asleep.can_stop && asleep.can_start);
        // Nobody looks: both are offered, unless a request is in flight.
        let away = tray_view(None, false);
        assert_eq!(away.tooltip, "Docklet");
        assert!(away.can_start && away.can_stop);
        let acting = tray_view(None, true);
        assert!(!acting.can_start && !acting.can_stop);
    }

    #[test]
    fn a_confirmation_is_a_question_and_what_follows_from_it() {
        assert_eq!(
            question("Remove 3 containers? 1 is running and will be stopped."),
            (
                "Remove 3 containers?".to_string(),
                "1 is running and will be stopped.".to_string()
            )
        );
        assert_eq!(
            question("No question"),
            ("No question".to_string(), String::new())
        );

        let mut list = containers();
        list.select_all();
        let confirm = removal_confirm(7, &list.removal_plan());
        assert_eq!(confirm.token, 7);
        assert_eq!(confirm.title, "Remove 3 containers?");
        assert_eq!(
            confirm.message,
            "2 are running and will be stopped. This cannot be undone."
        );
        assert_eq!(confirm.accept, "Remove 3");

        let prune = prune_confirm(8, PruneTarget::UnusedVolumes);
        assert_eq!(prune.title, "Remove every volume that no container uses?");
        assert_eq!(
            prune.message,
            "Their data will be deleted. This cannot be undone."
        );
    }

    #[test]
    fn a_removal_that_left_something_names_each_failure() {
        let mut list = containers();
        list.select_all();
        let plan = list.removal_plan();
        let report = RemoveReport {
            results: vec![
                RemoveResult::removed(ResourceKind::Container, "id-db"),
                RemoveResult::failed(
                    ResourceKind::Container,
                    "id-web",
                    RemoveFailure::Other,
                    "driver failed",
                ),
                RemoveResult::removed(ResourceKind::Container, "id-solo"),
            ],
        };
        let notice = removal_notice(&plan, &report);
        assert_eq!(notice.title, "Not everything was removed");
        assert!(notice.message.starts_with("Removed 2."));
        assert_eq!(notice.detail.as_deref(), Some("web: driver failed"));
        assert_eq!(notice.tone, view::Tone::Warning);
    }

    #[test]
    fn disk_usage_is_told_kind_by_kind() {
        let disk = DiskUsage {
            image_totals: DiskTotals {
                count: 12,
                active: 3,
                size: 6_000_000_000,
                reclaimable: 2_000_000_000,
            },
            ..Default::default()
        };
        let notice = disk_notice(&disk);
        assert_eq!(notice.title, "Docker keeps 6.0 GB on disk");
        assert!(notice.message.starts_with("2.0 GB of it can be reclaimed"));
        let detail = notice.detail.unwrap();
        assert!(detail.starts_with("Images: 6.0 GB in 12, 3 in use, 2.0 GB reclaimable"));
        assert_eq!(detail.lines().count(), 4);
    }

    #[test]
    fn the_pull_line_is_disabled_while_an_image_is_pulled() {
        let idle = pull_entry(false, "");
        assert_eq!((idle.button.as_str(), idle.enabled), ("Pull", true));
        let pulling = pull_entry(true, "a3ed95caeb02 Downloading 45%");
        assert_eq!(
            (pulling.button.as_str(), pulling.enabled),
            ("Pulling…", false)
        );
        assert_eq!(pulling.status, "a3ed95caeb02 Downloading 45%");
    }
}
