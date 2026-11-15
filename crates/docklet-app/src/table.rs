//! Containers, images, volumes and networks as tables of text, so a shell
//! can draw them without knowing Docker.

use std::cmp::Ordering;

use docklet_engine::models::{ContainerSummary, ImageSummary, Network, Volume};
use docklet_engine::{ContainerAction, ContainerUsage, DiskUsage, ResourceKind};

use crate::format::{
    UNKNOWN, compose_project, container_name, format_bytes, format_date, format_percent,
    image_label, is_running, published_ports, short_id, state_label,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub title: &'static str,
    pub align: Align,
    /// Relative width: the column gets `width` parts of the sum of all
    /// widths.
    pub width: u16,
}

const fn left(title: &'static str, width: u16) -> Column {
    Column {
        title,
        align: Align::Left,
        width,
    }
}

const fn right(title: &'static str, width: u16) -> Column {
    Column {
        title,
        align: Align::Right,
        width,
    }
}

/// The columns of the containers table, by index.
pub mod container_column {
    pub const NAME: usize = 0;
    pub const IMAGE: usize = 1;
    pub const STATE: usize = 2;
    pub const PORTS: usize = 3;
    /// Percent of one CPU, as `docker stats` shows it.
    pub const CPU: usize = 4;
    pub const MEMORY: usize = 5;
    /// Bytes received and sent over the network.
    pub const NET_IO: usize = 6;
    /// Bytes read from and written to disk.
    pub const DISK_IO: usize = 7;
}

/// The columns of the volumes table, by index.
pub mod volume_column {
    pub const NAME: usize = 0;
    pub const DRIVER: usize = 1;
    pub const SIZE: usize = 2;
}

const CONTAINER_COLUMNS: [Column; 8] = [
    left("Name", 22),
    left("Image", 20),
    left("State", 14),
    left("Ports", 10),
    right("CPU", 7),
    right("Memory", 9),
    right("Net I/O", 14),
    right("Disk I/O", 14),
];
const IMAGE_COLUMNS: [Column; 4] = [
    left("Name", 40),
    left("ID", 14),
    right("Size", 10),
    left("Created", 12),
];
const VOLUME_COLUMNS: [Column; 3] = [left("Name", 50), left("Driver", 14), right("Size", 10)];
const NETWORK_COLUMNS: [Column; 3] = [left("Name", 40), left("Driver", 14), left("Scope", 12)];

/// Networks Docker creates itself and refuses to remove.
const PREDEFINED_NETWORKS: [&str; 3] = ["bridge", "host", "none"];

/// What can be done to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowAction {
    Start,
    Stop,
    Restart,
    /// Removes the item; a running container is killed first.
    Remove,
    /// Shows the container's logs.
    Logs,
    /// Opens `format::port_url` of this published port in the browser.
    OpenPort(u16),
}

impl RowAction {
    pub fn label(self) -> String {
        match self {
            Self::Start => "Start".to_string(),
            Self::Stop => "Stop".to_string(),
            Self::Restart => "Restart".to_string(),
            Self::Remove => "Remove".to_string(),
            Self::Logs => "Logs".to_string(),
            Self::OpenPort(port) => format!("Open :{port}"),
        }
    }

    /// The engine call behind the action, for the four that are one.
    pub fn container_action(self) -> Option<ContainerAction> {
        match self {
            Self::Start => Some(ContainerAction::Start),
            Self::Stop => Some(ContainerAction::Stop),
            Self::Restart => Some(ContainerAction::Restart),
            Self::Remove => Some(ContainerAction::Remove),
            Self::Logs | Self::OpenPort(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowFlags {
    /// A container that runs: the green dot.
    pub running: bool,
    /// The row can be selected and removed. False for what Docker would
    /// refuse anyway, such as its predefined networks.
    pub removable: bool,
    /// A container is known to use the image or the volume, so removing it
    /// will be refused. False also means "not known".
    pub in_use: bool,
}

/// What a column is sorted by. A cell's text cannot do it: `9 MB` would sort
/// after `10 GB`.
#[derive(Debug, Clone, PartialEq)]
enum SortKey {
    /// No value: sorts after everything, in either direction.
    Missing,
    Number(f64),
    /// Lower case, so the order ignores case.
    Text(String),
}

impl SortKey {
    fn compare(&self, other: &Self, direction: SortDirection) -> Ordering {
        let ordering = match (self, other) {
            (Self::Missing, Self::Missing) => return Ordering::Equal,
            (Self::Missing, _) => return Ordering::Greater,
            (_, Self::Missing) => return Ordering::Less,
            (Self::Number(a), Self::Number(b)) => a.total_cmp(b),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Number(_)) => Ordering::Greater,
        };
        match direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        }
    }
}

/// One cell while a row is put together: its text and what it sorts by.
struct Cell(String, SortKey);

fn text(value: impl Into<String>) -> Cell {
    let value = value.into();
    let key = if value.is_empty() {
        SortKey::Missing
    } else {
        SortKey::Text(value.to_lowercase())
    };
    Cell(value, key)
}

fn number(shown: String, value: f64) -> Cell {
    Cell(shown, SortKey::Number(value))
}

fn missing(shown: &str) -> Cell {
    Cell(shown.to_string(), SortKey::Missing)
}

/// One row of a table.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// What selects the row and names it to the engine: a container, image
    /// or network ID, or a volume name.
    pub id: String,
    /// What the row is called in a sentence ("Remove web?").
    pub name: String,
    /// One text per column.
    pub cells: Vec<String>,
    /// The Compose project a container belongs to. Rows of one group are
    /// next to each other, groups first.
    pub group: Option<String>,
    pub flags: RowFlags,
    /// Published TCP ports of a container, to open in the browser.
    pub ports: Vec<u16>,
    /// What applies to the row as it is now.
    pub actions: Vec<RowAction>,
    keys: Vec<SortKey>,
}

impl Row {
    fn new(id: String, name: String, cells: Vec<Cell>) -> Self {
        let (cells, keys) = cells.into_iter().map(|Cell(text, key)| (text, key)).unzip();
        Self {
            id,
            name,
            cells,
            group: None,
            flags: RowFlags::default(),
            ports: Vec::new(),
            actions: Vec::new(),
            keys,
        }
    }

    fn set(&mut self, column: usize, cell: Cell) {
        self.cells[column] = cell.0;
        self.keys[column] = cell.1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub direction: SortDirection,
}

/// The text typed into a filter box. A row matches when every word occurs
/// in one of its cells or in its group's name, whatever the case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    words: Vec<String>,
}

impl Filter {
    pub fn new(text: &str) -> Self {
        Self {
            words: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn matches(&self, row: &Row) -> bool {
        let fields = || row.cells.iter().chain(row.group.as_ref());
        self.words
            .iter()
            .all(|word| fields().any(|field| field.to_lowercase().contains(word)))
    }
}

/// A list of one kind of resource, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub kind: ResourceKind,
    pub columns: Vec<Column>,
    /// In the default order: containers by Compose project and name, the
    /// rest as the engine listed them.
    pub rows: Vec<Row>,
}

impl Table {
    /// A table of `kind` without rows.
    pub fn empty(kind: ResourceKind) -> Self {
        let columns = match kind {
            ResourceKind::Container => CONTAINER_COLUMNS.to_vec(),
            ResourceKind::Image => IMAGE_COLUMNS.to_vec(),
            ResourceKind::Volume => VOLUME_COLUMNS.to_vec(),
            ResourceKind::Network => NETWORK_COLUMNS.to_vec(),
        };
        Self {
            kind,
            columns,
            rows: Vec::new(),
        }
    }

    /// What to show in place of a table without rows.
    pub fn empty_text(&self) -> &'static str {
        match self.kind {
            ResourceKind::Container => "No containers.",
            ResourceKind::Image => "No images.",
            ResourceKind::Volume => "No volumes.",
            ResourceKind::Network => "No networks.",
        }
    }

    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// The indexes of the rows that match `filter`, in the order to show
    /// them: the default order, or sorted by a column. Either way the rows
    /// of a group stay together and groups come first, by name, so a shell
    /// draws a group header wherever `group` changes.
    pub fn order(&self, sort: Option<Sort>, filter: &Filter) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.rows.len())
            .filter(|&index| filter.matches(&self.rows[index]))
            .collect();
        if let Some(sort) = sort.filter(|sort| sort.column < self.columns.len()) {
            // Stable, so rows with equal keys keep their default order.
            order.sort_by(|&a, &b| {
                let (a, b) = (&self.rows[a], &self.rows[b]);
                let by_key = || a.keys[sort.column].compare(&b.keys[sort.column], sort.direction);
                group_order(a, b).then_with(by_key)
            });
        }
        order
    }

    /// Puts the usage of the running containers into the usage columns of a
    /// containers table. `None` when no stats are taken. A running container
    /// without a figure shows `UNKNOWN`; one that does not run shows nothing.
    pub fn apply_usage(&mut self, usage: Option<&[ContainerUsage]>) {
        if self.kind != ResourceKind::Container {
            return;
        }
        for row in &mut self.rows {
            let sample = usage.and_then(|usage| usage.iter().find(|sample| sample.id == row.id));
            let cells = match sample {
                Some(sample) => usage_cells(sample),
                None if row.flags.running => [(); 4].map(|()| missing(UNKNOWN)),
                None => [(); 4].map(|()| missing("")),
            };
            for (offset, cell) in cells.into_iter().enumerate() {
                row.set(container_column::CPU + offset, cell);
            }
        }
    }
}

/// CPU, memory, network and disk cells of one container.
fn usage_cells(usage: &ContainerUsage) -> [Cell; 4] {
    let cpu = match usage.cpu_percent {
        Some(percent) => number(format_percent(percent), percent),
        // The first sample has nothing to compare with.
        None => missing(UNKNOWN),
    };
    let pair = |first: u64, second: u64| {
        let shown = format!("{} / {}", format_bytes(first), format_bytes(second));
        number(shown, first.saturating_add(second) as f64)
    };
    [
        cpu,
        number(format_bytes(usage.memory_used), usage.memory_used as f64),
        pair(usage.net_rx, usage.net_tx),
        pair(usage.block_read, usage.block_write),
    ]
}

/// Groups first, by name; rows without a group after them.
fn group_order(a: &Row, b: &Row) -> Ordering {
    match (&a.group, &b.group) {
        (Some(a), Some(b)) => compare_names(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Alphabetical whatever the case, with the exact text as tie-break.
fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The containers as a table: Compose projects first (by name), then
/// standalone containers (by name). `usage` fills the usage columns, see
/// `Table::apply_usage`.
pub fn containers_table(
    containers: &[ContainerSummary],
    usage: Option<&[ContainerUsage]>,
) -> Table {
    let mut table = Table::empty(ResourceKind::Container);
    table.rows = containers.iter().map(container_row).collect();
    table
        .rows
        .sort_by(|a, b| group_order(a, b).then_with(|| compare_names(&a.name, &b.name)));
    table.apply_usage(usage);
    table
}

fn container_row(container: &ContainerSummary) -> Row {
    let name = container_name(container);
    let running = is_running(container);
    let ports = published_ports(container.ports.as_deref());
    let shown_ports: Vec<String> = ports.iter().map(|port| format!(":{port}")).collect();
    let first_port = match ports.first() {
        Some(&port) => number(shown_ports.join(" "), f64::from(port)),
        None => missing(""),
    };
    let cells = vec![
        text(name.as_str()),
        text(container.image.as_deref().unwrap_or_default()),
        text(state_label(container)),
        first_port,
        missing(""),
        missing(""),
        missing(""),
        missing(""),
    ];
    let id = container.id.clone().unwrap_or_else(|| name.clone());
    let mut row = Row::new(id, name, cells);
    row.group = compose_project(container).map(str::to_string);
    row.flags = RowFlags {
        running,
        removable: true,
        in_use: false,
    };
    row.actions = if running {
        vec![RowAction::Stop, RowAction::Restart]
    } else {
        vec![RowAction::Start]
    };
    row.actions.extend([RowAction::Remove, RowAction::Logs]);
    row.actions
        .extend(ports.iter().map(|&port| RowAction::OpenPort(port)));
    row.ports = ports;
    row
}

/// The images as a table, in the engine's order (newest first).
pub fn images_table(images: &[ImageSummary]) -> Table {
    let mut table = Table::empty(ResourceKind::Image);
    table.rows = images.iter().map(image_row).collect();
    table
}

fn image_row(image: &ImageSummary) -> Row {
    let name = image_label(image);
    // Docker reports "not known" as -1.
    let size = u64::try_from(image.size).unwrap_or(0);
    let cells = vec![
        text(name.as_str()),
        text(short_id(&image.id)),
        number(format_bytes(size), size as f64),
        number(format_date(image.created), image.created as f64),
    ];
    let mut row = Row::new(image.id.clone(), name, cells);
    row.flags = RowFlags {
        running: false,
        removable: true,
        in_use: image.containers > 0,
    };
    row.actions = vec![RowAction::Remove];
    row
}

/// The volumes as a table. Sizes and whether a container uses a volume are
/// only known from the engine's disk usage, which is worked out on demand;
/// without it the size shows `UNKNOWN`.
pub fn volumes_table(volumes: &[Volume], disk: Option<&DiskUsage>) -> Table {
    let mut table = Table::empty(ResourceKind::Volume);
    table.rows = volumes
        .iter()
        .map(|volume| volume_row(volume, disk))
        .collect();
    table
}

fn volume_row(volume: &Volume, disk: Option<&DiskUsage>) -> Row {
    let measured = disk.and_then(|disk| disk.volumes.iter().find(|v| v.name == volume.name));
    let listed = volume.usage_data.as_ref();
    let size = match measured {
        Some(measured) => measured.size,
        None => listed.and_then(|usage| u64::try_from(usage.size).ok()),
    };
    let users = match measured {
        Some(measured) => measured.ref_count,
        None => listed.and_then(|usage| u64::try_from(usage.ref_count).ok()),
    };
    let cells = vec![
        text(volume.name.as_str()),
        text(volume.driver.as_str()),
        match size {
            Some(size) => number(format_bytes(size), size as f64),
            None => missing(UNKNOWN),
        },
    ];
    let mut row = Row::new(volume.name.clone(), volume.name.clone(), cells);
    row.flags = RowFlags {
        running: false,
        removable: true,
        in_use: users.is_some_and(|users| users > 0),
    };
    row.actions = vec![RowAction::Remove];
    row
}

/// The networks as a table, in the engine's order.
pub fn networks_table(networks: &[Network]) -> Table {
    let mut table = Table::empty(ResourceKind::Network);
    table.rows = networks
        .iter()
        .enumerate()
        .map(|(index, network)| network_row(index, network))
        .collect();
    table
}

fn network_row(index: usize, network: &Network) -> Row {
    let id = network
        .id
        .clone()
        .or_else(|| network.name.clone())
        .unwrap_or_else(|| index.to_string());
    let name = network
        .name
        .clone()
        .unwrap_or_else(|| id.chars().take(12).collect());
    let cells = vec![
        text(name.as_str()),
        text(network.driver.as_deref().unwrap_or_default()),
        text(network.scope.as_deref().unwrap_or_default()),
    ];
    // Without an ID there is nothing to name to the engine.
    let removable = network.id.is_some() && !PREDEFINED_NETWORKS.contains(&name.as_str());
    let mut row = Row::new(id, name, cells);
    row.flags = RowFlags {
        running: false,
        removable,
        in_use: false,
    };
    if removable {
        row.actions = vec![RowAction::Remove];
    }
    row
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;

    use docklet_engine::models::ContainerSummaryStateEnum;

    use super::*;

    /// A container named `name` with the ID `id-<name>`.
    pub(crate) fn container(name: &str, project: Option<&str>, running: bool) -> ContainerSummary {
        let labels = project.map(|project| {
            let label = "com.docker.compose.project".to_string();
            HashMap::from([(label, project.to_string())])
        });
        ContainerSummary {
            id: Some(format!("id-{name}")),
            names: Some(vec![format!("/{name}")]),
            image: Some(format!("{name}:latest")),
            state: Some(if running {
                ContainerSummaryStateEnum::RUNNING
            } else {
                ContainerSummaryStateEnum::EXITED
            }),
            labels,
            ..Default::default()
        }
    }

    pub(crate) fn usage(name: &str, cpu: Option<f64>, memory: u64) -> ContainerUsage {
        ContainerUsage {
            id: format!("id-{name}"),
            name: name.to_string(),
            cpu_percent: cpu,
            online_cpus: 4,
            memory_used: memory,
            memory_limit: 8_000_000_000,
            memory_percent: Some(memory as f64 / 8e7),
            net_rx: 1_200_000,
            net_tx: 340_000,
            block_read: 4_100_000,
            block_write: 12_000,
            pids: 3,
        }
    }

    pub(crate) fn image(id: &str, tags: &[&str], size: i64, created: i64) -> ImageSummary {
        ImageSummary {
            id: id.to_string(),
            repo_tags: tags.iter().map(|tag| tag.to_string()).collect(),
            size,
            created,
            containers: -1,
            ..Default::default()
        }
    }

    pub(crate) fn volume(name: &str) -> Volume {
        Volume {
            name: name.to_string(),
            driver: "local".to_string(),
            ..Default::default()
        }
    }

    pub(crate) fn network(name: &str) -> Network {
        Network {
            id: Some(format!("net-{name}")),
            name: Some(name.to_string()),
            driver: Some("bridge".to_string()),
            scope: Some("local".to_string()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use docklet_engine::VolumeDisk;
    use docklet_engine::models::{PortSummary, PortSummaryTypeEnum, VolumeUsageData};

    use super::testing::{container, image, network, usage, volume};
    use super::*;

    fn names(table: &Table) -> Vec<&str> {
        table.rows.iter().map(|row| row.name.as_str()).collect()
    }

    fn ordered<'a>(table: &'a Table, sort: Option<Sort>, filter: &str) -> Vec<&'a str> {
        let order = table.order(sort, &Filter::new(filter));
        order
            .into_iter()
            .map(|index| table.rows[index].name.as_str())
            .collect()
    }

    fn sort(column: usize, direction: SortDirection) -> Option<Sort> {
        Some(Sort { column, direction })
    }

    fn mixed_containers() -> Vec<ContainerSummary> {
        vec![
            container("solo-b", None, false),
            container("web", Some("shop"), true),
            container("solo-a", None, true),
            container("db", Some("shop"), true),
            container("api", Some("alpha"), false),
        ]
    }

    #[test]
    fn groups_by_compose_project_projects_first_sorted_by_name() {
        let table = containers_table(&mixed_containers(), None);
        let rows: Vec<String> = table
            .rows
            .iter()
            .map(|row| match &row.group {
                Some(project) => format!("{project}:{}", row.name),
                None => row.name.clone(),
            })
            .collect();
        assert_eq!(
            rows,
            ["alpha:api", "shop:db", "shop:web", "solo-a", "solo-b"]
        );
    }

    #[test]
    fn returns_nothing_for_no_containers() {
        let table = containers_table(&[], None);
        assert!(table.rows.is_empty());
        assert_eq!(table.empty_text(), "No containers.");
        assert_eq!(table.columns.len(), 8);
    }

    #[test]
    fn every_row_has_a_cell_for_every_column() {
        let tables = [
            containers_table(&mixed_containers(), None),
            images_table(&[image("sha256:abc", &["a:1"], 10, 0)]),
            volumes_table(&[volume("data")], None),
            networks_table(&[network("bridge")]),
        ];
        for table in tables {
            for row in &table.rows {
                assert_eq!(row.cells.len(), table.columns.len());
                assert_eq!(row.keys.len(), table.columns.len());
            }
        }
    }

    #[test]
    fn a_container_row_shows_name_image_state_and_ports() {
        let mut web = container("web", None, true);
        web.status = Some("Up 5 minutes (healthy)".to_string());
        let port = |public| PortSummary {
            ip: None,
            private_port: 80,
            public_port: Some(public),
            typ: Some(PortSummaryTypeEnum::TCP),
        };
        web.ports = Some(vec![port(8080), port(443), port(8080)]);
        let table = containers_table(&[web], None);
        let row = &table.rows[0];
        assert_eq!(row.id, "id-web");
        assert_eq!(
            row.cells[..4],
            ["web", "web:latest", "running (healthy)", ":443 :8080"]
        );
        assert_eq!(row.ports, [443, 8080]);
        assert!(row.flags.running);
    }

    #[test]
    fn the_actions_follow_from_whether_the_container_runs() {
        use RowAction::{Logs, OpenPort, Remove, Restart, Start, Stop};
        let mut web = container("web", None, true);
        web.ports = Some(vec![PortSummary {
            ip: None,
            private_port: 80,
            public_port: Some(8080),
            typ: None,
        }]);
        let table = containers_table(&[web, container("db", None, false)], None);
        assert_eq!(table.row("id-db").unwrap().actions, [Start, Remove, Logs]);
        assert_eq!(
            table.row("id-web").unwrap().actions,
            [Stop, Restart, Remove, Logs, OpenPort(8080)]
        );
    }

    #[test]
    fn actions_name_themselves_and_their_engine_call() {
        assert_eq!(RowAction::Restart.label(), "Restart");
        assert_eq!(RowAction::OpenPort(8080).label(), "Open :8080");
        assert_eq!(
            RowAction::Remove.container_action(),
            Some(ContainerAction::Remove)
        );
        assert_eq!(RowAction::Logs.container_action(), None);
        assert_eq!(RowAction::OpenPort(80).container_action(), None);
    }

    #[test]
    fn usage_is_merged_into_the_rows_of_running_containers() {
        let containers = [
            container("web", None, true),
            container("db", None, true),
            container("old", None, false),
        ];
        let samples = [usage("web", Some(12.54), 52_400_000)];
        let table = containers_table(&containers, Some(&samples));
        let cells = |id: &str| table.row(id).unwrap().cells[container_column::CPU..].to_vec();
        assert_eq!(
            cells("id-web"),
            ["12.5%", "52.4 MB", "1.2 MB / 340 KB", "4.1 MB / 12.0 KB"]
        );
        // Running, but its first sample has not arrived.
        assert_eq!(cells("id-db"), [UNKNOWN; 4]);
        assert_eq!(cells("id-old"), [""; 4]);
    }

    #[test]
    fn the_first_sample_of_a_container_has_no_cpu_figure() {
        let samples = [usage("web", None, 1_000_000)];
        let table = containers_table(&[container("web", None, true)], Some(&samples));
        let row = &table.rows[0];
        assert_eq!(row.cells[container_column::CPU], UNKNOWN);
        assert_eq!(row.cells[container_column::MEMORY], "1.0 MB");
    }

    #[test]
    fn usage_that_stops_coming_is_taken_out_again() {
        let samples = [usage("web", Some(1.0), 1_000_000)];
        let mut table = containers_table(&[container("web", None, true)], Some(&samples));
        table.apply_usage(None);
        assert_eq!(table.rows[0].cells[container_column::CPU], UNKNOWN);
        assert_eq!(table.rows[0].cells[container_column::MEMORY], UNKNOWN);
    }

    #[test]
    fn usage_does_not_touch_other_tables() {
        let mut table = images_table(&[image("sha256:abc", &["a:1"], 10, 0)]);
        let before = table.clone();
        table.apply_usage(Some(&[usage("a", Some(1.0), 1)]));
        assert_eq!(table, before);
    }

    #[test]
    fn sorting_by_a_column_keeps_groups_together() {
        let table = containers_table(&mixed_containers(), None);
        let descending = sort(container_column::NAME, SortDirection::Descending);
        assert_eq!(
            ordered(&table, descending, ""),
            ["api", "web", "db", "solo-b", "solo-a"]
        );
        let by_state = sort(container_column::STATE, SortDirection::Descending);
        // `running` before `exited`; the rows of `shop` are both running and
        // keep their order by name.
        assert_eq!(
            ordered(&table, by_state, ""),
            ["api", "db", "web", "solo-a", "solo-b"]
        );
    }

    #[test]
    fn sizes_sort_by_their_value_not_their_text() {
        let table = images_table(&[
            image("sha256:a", &["nine:1"], 9_000_000, 30),
            image("sha256:b", &["ten:1"], 10_000_000_000, 10),
            image("sha256:c", &["half:1"], 500, 20),
        ]);
        assert_eq!(
            ordered(&table, sort(2, SortDirection::Ascending), ""),
            ["half:1", "nine:1", "ten:1"]
        );
        assert_eq!(
            ordered(&table, sort(2, SortDirection::Descending), ""),
            ["ten:1", "nine:1", "half:1"]
        );
        // Created: oldest first.
        assert_eq!(
            ordered(&table, sort(3, SortDirection::Ascending), ""),
            ["ten:1", "half:1", "nine:1"]
        );
    }

    #[test]
    fn rows_without_a_figure_sort_last_in_both_directions() {
        let containers = [
            container("idle", None, true),
            container("busy", None, true),
            container("new", None, true),
        ];
        let samples = [
            usage("idle", Some(0.5), 1),
            usage("busy", Some(90.0), 1),
            usage("new", None, 1),
        ];
        let table = containers_table(&containers, Some(&samples));
        let cpu = |direction| sort(container_column::CPU, direction);
        assert_eq!(
            ordered(&table, cpu(SortDirection::Descending), ""),
            ["busy", "idle", "new"]
        );
        assert_eq!(
            ordered(&table, cpu(SortDirection::Ascending), ""),
            ["idle", "busy", "new"]
        );
    }

    #[test]
    fn text_sorts_whatever_the_case() {
        let table = volumes_table(&[volume("beta"), volume("Alpha"), volume("gamma")], None);
        assert_eq!(
            ordered(&table, sort(0, SortDirection::Ascending), ""),
            ["Alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn a_column_that_does_not_exist_sorts_nothing() {
        let table = volumes_table(&[volume("b"), volume("a")], None);
        assert_eq!(
            ordered(&table, sort(9, SortDirection::Ascending), ""),
            ["b", "a"]
        );
    }

    #[test]
    fn the_filter_matches_every_word_in_any_cell_or_the_group() {
        let table = containers_table(&mixed_containers(), None);
        assert_eq!(ordered(&table, None, "SOLO"), ["solo-a", "solo-b"]);
        // The project's name finds its members.
        assert_eq!(ordered(&table, None, "shop"), ["db", "web"]);
        assert_eq!(ordered(&table, None, "shop running db"), ["db"]);
        assert_eq!(ordered(&table, None, "exited"), ["api", "solo-b"]);
        assert_eq!(ordered(&table, None, "nothing-like-it"), [] as [&str; 0]);
        assert_eq!(ordered(&table, None, "   ").len(), 5);
        assert!(Filter::new("  ").is_empty());
    }

    #[test]
    fn images_show_tags_short_id_size_and_date() {
        let table = images_table(&[
            image(
                "sha256:0123456789abcdef",
                &["nginx:1.29", "nginx:latest"],
                412_000_000,
                1_700_000_000,
            ),
            image("sha256:fedcba9876543210", &["<none>:<none>"], -1, 0),
        ]);
        assert_eq!(
            table.rows[0].cells,
            [
                "nginx:1.29, nginx:latest",
                "0123456789ab",
                "412 MB",
                "2023-11-14"
            ]
        );
        assert_eq!(table.rows[0].id, "sha256:0123456789abcdef");
        assert_eq!(table.rows[1].name, "fedcba987654");
        assert_eq!(table.rows[1].cells[2], "0 B");
        assert_eq!(table.rows[0].actions, [RowAction::Remove]);
    }

    #[test]
    fn an_image_with_containers_is_marked_in_use() {
        let mut used = image("sha256:a", &["a:1"], 1, 0);
        used.containers = 2;
        let table = images_table(&[used, image("sha256:b", &["b:1"], 1, 0)]);
        assert!(table.rows[0].flags.in_use);
        // -1: the engine did not count.
        assert!(!table.rows[1].flags.in_use);
    }

    #[test]
    fn volume_sizes_come_from_the_disk_usage() {
        let volumes = [volume("data"), volume("cache"), volume("remote")];
        let without = volumes_table(&volumes, None);
        assert_eq!(without.rows[0].cells, ["data", "local", UNKNOWN]);
        assert!(!without.rows[0].flags.in_use);

        let measured = |name: &str, size, ref_count| VolumeDisk {
            name: name.to_string(),
            driver: "local".to_string(),
            size,
            ref_count,
        };
        let disk = DiskUsage {
            volumes: vec![
                measured("data", Some(2_340_000_000), Some(1)),
                measured("cache", Some(0), Some(0)),
                measured("remote", None, None),
            ],
            ..Default::default()
        };
        let with = volumes_table(&volumes, Some(&disk));
        let sizes: Vec<&str> = with
            .rows
            .iter()
            .map(|row| row.cells[volume_column::SIZE].as_str())
            .collect();
        assert_eq!(sizes, ["2.3 GB", "0 B", UNKNOWN]);
        assert!(with.rows[0].flags.in_use);
        assert!(!with.rows[1].flags.in_use);
    }

    #[test]
    fn a_volume_listed_with_its_usage_shows_it() {
        let mut data = volume("data");
        data.usage_data = Some(VolumeUsageData {
            size: 1500,
            ref_count: -1,
        });
        let table = volumes_table(&[data], None);
        assert_eq!(table.rows[0].cells[volume_column::SIZE], "1.5 KB");
        assert!(!table.rows[0].flags.in_use);
    }

    #[test]
    fn networks_fall_back_from_id_to_name_to_position() {
        let nameless = Network {
            id: Some("0123456789abcdef".to_string()),
            ..Default::default()
        };
        let idless = Network {
            name: Some("orphan".to_string()),
            ..Default::default()
        };
        let table = networks_table(&[network("app"), nameless, idless, Network::default()]);
        assert_eq!(table.rows[0].id, "net-app");
        assert_eq!(table.rows[0].cells, ["app", "bridge", "local"]);
        assert_eq!(table.rows[1].name, "0123456789ab");
        assert_eq!(table.rows[2].id, "orphan");
        assert_eq!(table.rows[3].id, "3");
        // Nothing to name to the engine without an ID.
        assert!(table.rows[0].flags.removable);
        assert!(!table.rows[2].flags.removable);
        assert!(table.rows[2].actions.is_empty());
    }

    #[test]
    fn the_predefined_networks_cannot_be_removed() {
        let table = networks_table(&[
            network("bridge"),
            network("host"),
            network("none"),
            network("shop_default"),
        ]);
        let removable: Vec<&str> = table
            .rows
            .iter()
            .filter(|row| row.flags.removable)
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(removable, ["shop_default"]);
        assert_eq!(names(&table).len(), 4);
    }
}
