//! Fake data through the boundary, for looking at the window and measuring
//! it: four tables of a few hundred rows, usage meters that change every
//! second, and a log of 10,000 lines a second.
//!
//! The meters and the log run on their own threads, and only while the
//! window is shown: they block on a condition variable otherwise.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::boundary::{
    Action, Banner, Button, Column, Confirm, Header, LogPane, LogRing, Meter, MeterValue, Notice,
    Page, Progress, Row, RowFlags, Screen, SortBy, Table, Tone, TrayCommand, TrayState, UiEvent,
    UiHandle, UiUpdate,
};

pub const LINES_PER_SECOND: u64 = 10_000;
/// How often the log generator wakes. Windows may stretch it to its 15.6 ms
/// tick, so the batch size comes from the clock.
const TICK: Duration = Duration::from_millis(10);
/// Most lines one wake-up adds, so a thread that was held up does not catch
/// up in one burst.
const MAX_BURST: u64 = 2_000;

#[derive(Clone, Debug, Default)]
pub struct DemoOptions {
    /// Open a log at once.
    pub logs: bool,
    /// Show one message screen instead of the tables: wsl, install,
    /// installing, stopped, sleeping, starting or error.
    pub screen: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A switch other threads wait on.
struct Gate {
    open: Mutex<bool>,
    wake: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            wake: Condvar::new(),
        })
    }

    fn set(&self, open: bool) {
        let mut current = lock(&self.open);
        if *current != open {
            *current = open;
            self.wake.notify_all();
        }
    }

    fn is_open(&self) -> bool {
        *lock(&self.open)
    }

    /// Blocks while the gate is closed.
    fn wait(&self) {
        let mut open = lock(&self.open);
        while !*open {
            open = self.wake.wait(open).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Writes fake log lines into the ring it is given while its gate is open.
struct LogFeed {
    gate: Arc<Gate>,
    ring: Mutex<Option<Arc<LogRing>>>,
}

impl LogFeed {
    fn spawn() -> Arc<Self> {
        let feed = Arc::new(Self {
            gate: Gate::new(),
            ring: Mutex::new(None),
        });
        let worker = Arc::clone(&feed);
        thread::spawn(move || worker.generate());
        feed
    }

    fn generate(&self) {
        let mut sequence = 0u64;
        loop {
            self.gate.wait();
            let started = Instant::now();
            let mut accounted = 0u64;
            while self.gate.is_open() {
                let due = started.elapsed().as_millis() as u64 * LINES_PER_SECOND / 1000;
                let batch = (due - accounted).min(MAX_BURST);
                accounted = due;
                let ring = lock(&self.ring).clone();
                if let Some(ring) = ring {
                    ring.push_batch(batch as usize, |line| {
                        fake_line(line, sequence);
                        sequence += 1;
                    });
                }
                thread::sleep(TICK);
            }
        }
    }
}

fn fake_line(line: &mut String, sequence: u64) {
    const LEVELS: [&str; 4] = [
        "\x1b[32mINFO \x1b[0m",
        "\x1b[90mDEBUG\x1b[0m",
        "\x1b[33mWARN \x1b[0m",
        "\x1b[1;31mERROR\x1b[0m",
    ];
    const PATHS: [&str; 5] = [
        "/api/v1/orders",
        "/api/v1/items/42",
        "/healthz",
        "/api/v1/users/7/cart",
        "/static/app.js",
    ];
    let millis = sequence / 10;
    let level = match sequence % 50 {
        0 => 3,
        1..=4 => 2,
        5..=14 => 1,
        _ => 0,
    };
    let _ = write!(
        line,
        "2026-11-15T12:{:02}:{:02}.{:03}Z {} [worker-{}] #{sequence} GET {} \x1b[36m200\x1b[0m {}ms 한글 로그",
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000,
        LEVELS[level],
        sequence % 8,
        PATHS[(sequence % 5) as usize],
        sequence % 97,
    );
}

struct Container {
    id: String,
    name: String,
    image: &'static str,
    project: Option<u32>,
    running: bool,
    port: Option<u16>,
    cpu: u32,
    memory: u32,
}

const PROJECTS: [&str; 8] = [
    "shop", "blog", "metrics", "auth", "search", "mail", "ci", "위키",
];
const SERVICES: [&str; 5] = ["web", "api", "db", "cache", "worker"];
const IMAGES: [&str; 5] = [
    "nginx:1.29",
    "node:24-alpine",
    "postgres:18",
    "redis:8",
    "python:3.14-slim",
];

fn containers() -> Vec<Container> {
    let mut list = Vec::new();
    for n in 0..240usize {
        let service = n % SERVICES.len();
        let grouped = n < 200;
        let name = if grouped {
            format!("{}-{}-{}", PROJECTS[n / 25], SERVICES[service], n % 25 + 1)
        } else if n % 7 == 0 {
            format!("실험-{}", n - 199)
        } else {
            format!("scratch-{}", n - 199)
        };
        list.push(Container {
            id: format!(
                "{:012x}",
                0x9e37_79b9_7f4a_u64.wrapping_mul(n as u64 + 1) & 0xffff_ffff_ffff
            ),
            name,
            image: IMAGES[service],
            project: grouped.then_some((n / 25) as u32),
            running: n % 3 != 0,
            port: (service < 2).then_some(8000 + n as u16),
            cpu: (n * 37 % 400) as u32,
            memory: (20 + n * 53 % 900) as u32,
        });
    }
    list
}

const ACTION_START: usize = 0;
const ACTION_STOP: usize = 1;
const ACTION_RESTART: usize = 2;
const ACTION_LOGS: usize = 3;
const ACTION_REMOVE: usize = 4;

fn container_table(list: &[Container]) -> Table {
    let rows = list
        .iter()
        .map(|container| {
            let mut row = Row::new(
                container.id.clone(),
                vec![
                    container.name.clone(),
                    container.image.to_string(),
                    if container.running {
                        "running"
                    } else {
                        "exited"
                    }
                    .to_string(),
                    container
                        .port
                        .map(|port| format!(":{port}"))
                        .unwrap_or_default(),
                    if container.running {
                        format!("{}.{}%", container.cpu / 10, container.cpu % 10)
                    } else {
                        String::new()
                    },
                    format!("{} MB", container.memory),
                ],
            );
            row.keys = vec![
                0,
                0,
                0,
                0,
                i64::from(container.cpu),
                i64::from(container.memory),
            ];
            row.group = container.project;
            row.flags = if container.running {
                RowFlags::ACTIVE
            } else {
                RowFlags::default()
            };
            let mut actions = 1 << ACTION_LOGS | 1 << ACTION_REMOVE;
            actions |= if container.running {
                1 << ACTION_STOP | 1 << ACTION_RESTART
            } else {
                1 << ACTION_START
            };
            row.actions = actions;
            row
        })
        .collect();
    Table {
        columns: vec![
            Column::new("Name", 3).min_width(140),
            Column::new("Image", 2).min_width(100),
            Column::new("State", 1).status(),
            Column::new("Ports", 1),
            Column::new("CPU", 1).right().sort(SortBy::Key),
            Column::new("Memory", 1).right().sort(SortBy::Key),
        ],
        groups: PROJECTS.iter().map(|project| project.to_string()).collect(),
        actions: vec![
            Action::new("start", "Start"),
            Action::new("stop", "Stop"),
            Action::new("restart", "Restart"),
            Action::new("logs", "Logs"),
            Action::remove("remove", "Remove"),
        ],
        default_action: Some(ACTION_LOGS),
        rows,
        noun: "containers".into(),
        empty_text: "No containers.".into(),
    }
}

/// A simple table with a Remove action.
struct Simple {
    id: String,
    cells: Vec<String>,
    keys: Vec<i64>,
}

fn simple_table(columns: Vec<Column>, rows: &[Simple], noun: &str) -> Table {
    Table {
        columns,
        actions: vec![Action::remove("remove", "Remove")],
        rows: rows
            .iter()
            .map(|simple| {
                let mut row = Row::new(simple.id.clone(), simple.cells.clone());
                row.keys = simple.keys.clone();
                row
            })
            .collect(),
        noun: noun.into(),
        empty_text: format!("No {noun}."),
        ..Table::default()
    }
}

fn images() -> Vec<Simple> {
    (0..60u64)
        .map(|n| {
            let size = 40 + n * 137 % 1900;
            Simple {
                id: format!(
                    "sha256:{:012x}",
                    0x51_7cc1_b727_u64.wrapping_mul(n + 3) & 0xffff_ffff_ffff
                ),
                cells: vec![
                    format!(
                        "registry.example.com/team/{}",
                        IMAGES[(n % 5) as usize].split(':').next().unwrap_or("")
                    ),
                    format!("v{}.{}", n % 4 + 1, n % 10),
                    format!(
                        "{:012x}",
                        0x51_7cc1_b727_u64.wrapping_mul(n + 3) & 0xffff_ffff_ffff
                    ),
                    format!("{size} MB"),
                    format!("{} days ago", n * 3 % 90 + 1),
                ],
                keys: vec![0, 0, 0, size as i64, -((n * 3 % 90 + 1) as i64)],
            }
        })
        .collect()
}

fn volumes() -> Vec<Simple> {
    (0..30usize)
        .map(|n| {
            let name = format!("{}_data{}", PROJECTS[n % 8], n / 8);
            Simple {
                id: name.clone(),
                cells: vec![
                    name.clone(),
                    "local".into(),
                    format!("/var/lib/docker/volumes/{name}/_data"),
                ],
                keys: Vec::new(),
            }
        })
        .collect()
}

fn networks() -> Vec<Simple> {
    (0..8usize)
        .map(|n| Simple {
            id: format!("net{n}"),
            cells: vec![
                format!("{}_default", PROJECTS[n]),
                "bridge".into(),
                format!("172.{}.0.0/16", 18 + n),
            ],
            keys: Vec::new(),
        })
        .collect()
}

fn meters(tick: u64) -> Vec<Meter> {
    let wave = |period: u64, phase: u64| {
        let position = (tick + phase) % period;
        let half = period / 2;
        let rising = if position < half {
            position
        } else {
            period - position
        };
        rising as f32 / half as f32
    };
    let memory = 1.2 + 5.6 * wave(40, 0);
    let cpu = 3.0 + 90.0 * wave(16, 5);
    let disk = 18.2 + (tick % 600) as f32 * 0.01;
    vec![
        Meter {
            label: "Memory".into(),
            value: MeterValue::Known {
                text: format!("{memory:.1} of 8.0 GB"),
                fill: memory / 8.0,
            },
        },
        Meter {
            label: "CPU".into(),
            value: MeterValue::Known {
                text: format!("{cpu:.0}%"),
                fill: cpu / 100.0,
            },
        },
        Meter {
            label: "GPU".into(),
            value: MeterValue::Unknown {
                reason: "No GPU counters for this VM".into(),
            },
        },
        Meter {
            label: "Disk".into(),
            value: MeterValue::Known {
                text: format!("{disk:.1} of 64 GB"),
                fill: disk / 64.0,
            },
        },
    ]
}

fn screen(name: &str) -> Screen {
    let button = |key: &str, label: &str| Some(Button::new(key, label));
    match name {
        "wsl" => Screen {
            title: "WSL is not installed".into(),
            body: "Docklet runs Docker in WSL 2. Install it from an administrator terminal, restart Windows, then check again.".into(),
            detail: Some("wsl --install --no-distribution".into()),
            primary: button("recheck", "Check again"),
            ..Screen::default()
        },
        "install" => Screen {
            title: "Set up the Docker engine".into(),
            body: "Docklet installs Docker Engine into its own WSL distro. Other distros are not touched.".into(),
            primary: button("install", "Install"),
            ..Screen::default()
        },
        "installing" => Screen {
            title: "Installing the Docker engine…".into(),
            body: "This downloads about 60 MB and takes a minute or two.".into(),
            detail: Some("Downloading rootfs… done\nImporting the distro… done\nStarting dockerd…".into()),
            progress: Progress::Fraction(0.6),
            ..Screen::default()
        },
        "sleeping" => Screen {
            title: "The engine is asleep".into(),
            body: "Nothing was using Docker, so Docklet stopped the engine to free its memory. It starts again when you start it here, and by itself with the next Docker command from anywhere.".into(),
            primary: button("start", "Start engine"),
            secondary: button("stop", "Stop engine"),
            ..Screen::default()
        },
        "starting" => Screen {
            title: "Starting the engine…".into(),
            body: "If this does not finish, start the engine again to repair it, or stop it.".into(),
            progress: Progress::Busy,
            primary: button("start", "Start engine"),
            secondary: button("stop", "Stop engine"),
            ..Screen::default()
        },
        "error" => Screen {
            title: "The engine hit a problem".into(),
            body: "Docklet could not reach dockerd.".into(),
            detail: Some("dockerd exited with status 1:\nfailed to start daemon: pid file found, ensure docker is not running or delete /var/run/docker.pid".into()),
            primary: button("retry", "Retry"),
            ..Screen::default()
        },
        _ => Screen {
            title: "The engine is stopped".into(),
            body: "Start it to see containers, images, volumes and networks.".into(),
            primary: button("start", "Start engine"),
            ..Screen::default()
        },
    }
}

/// The demo's side of the boundary: owns the fake data and answers events.
pub struct Demo {
    handle: UiHandle,
    containers: Vec<Container>,
    others: [Vec<Simple>; 3],
    running: bool,
    visible: Arc<Gate>,
    feed: Arc<LogFeed>,
    log_open: bool,
    next_token: u64,
    pending: HashMap<u64, (usize, Vec<String>)>,
}

impl Demo {
    /// Sends the first data and starts the meter and log threads.
    pub fn start(handle: UiHandle, options: &DemoOptions) -> Self {
        let visible = Gate::new();
        let meters_gate = Arc::clone(&visible);
        let meters_handle = handle.clone();
        thread::spawn(move || {
            let mut tick = 0u64;
            loop {
                meters_gate.wait();
                meters_handle.send(UiUpdate::Usage(meters(tick)));
                tick += 1;
                thread::sleep(Duration::from_secs(1));
            }
        });
        let mut demo = Self {
            handle,
            containers: containers(),
            others: [images(), volumes(), networks()],
            running: options.screen.is_none(),
            visible,
            feed: LogFeed::spawn(),
            log_open: false,
            next_token: 1,
            pending: HashMap::new(),
        };
        demo.send_state();
        demo.handle.send(UiUpdate::Banner(Some(Banner {
            text: "Demo mode: the data is made up and nothing touches Docker.".into(),
            tone: Tone::Info,
        })));
        if let Some(name) = &options.screen {
            demo.handle
                .send(UiUpdate::Page(Page::Message(screen(name))));
        }
        if options.logs {
            demo.open_log(0);
        }
        demo
    }

    /// The header and the tray, which follow the engine.
    fn send_chrome(&self) {
        let status = if self.running {
            "Engine running"
        } else {
            "Engine stopped"
        };
        self.handle.send(UiUpdate::Header(Header {
            status: status.into(),
            button: self.running.then(|| Button::new("stop", "Stop engine")),
        }));
        self.handle.send(UiUpdate::Tray(TrayState {
            tooltip: format!("Docklet: {}", status.to_lowercase()),
            can_start: !self.running,
            can_stop: self.running,
        }));
    }

    fn send_state(&self) {
        self.send_chrome();
        self.handle.send(UiUpdate::Page(if self.running {
            Page::Tables
        } else {
            Page::Message(screen("stopped"))
        }));
        // No tables: like the real app, the lists are fetched on
        // WindowShown, so the tray alone holds none.
    }

    fn send_tables(&self) {
        let titles = [
            format!("Containers ({})", self.containers.len()),
            format!("Images ({})", self.others[0].len()),
            format!("Volumes ({})", self.others[1].len()),
            format!("Networks ({})", self.others[2].len()),
        ];
        self.handle.send(UiUpdate::Tabs(titles.to_vec()));
        for tab in 0..4 {
            self.send_table(tab);
        }
    }

    fn send_table(&self, tab: usize) {
        let table = match tab {
            0 => container_table(&self.containers),
            1 => simple_table(
                vec![
                    Column::new("Repository", 4).min_width(160),
                    Column::new("Tag", 1),
                    Column::new("ID", 2),
                    Column::new("Size", 1).right().sort(SortBy::Key),
                    Column::new("Created", 1).right().sort(SortBy::Key),
                ],
                &self.others[0],
                "images",
            ),
            2 => simple_table(
                vec![
                    Column::new("Name", 2).min_width(120),
                    Column::new("Driver", 1),
                    Column::new("Mountpoint", 4),
                ],
                &self.others[1],
                "volumes",
            ),
            _ => simple_table(
                vec![
                    Column::new("Name", 2).min_width(120),
                    Column::new("Driver", 1),
                    Column::new("Subnet", 2),
                ],
                &self.others[2],
                "networks",
            ),
        };
        self.handle.send(UiUpdate::Table { tab, table });
    }

    fn open_log(&mut self, index: usize) {
        let Some(container) = self.containers.get(index) else {
            return;
        };
        let ring = Arc::new(LogRing::new(LogRing::DEFAULT_CAPACITY));
        *lock(&self.feed.ring) = Some(Arc::clone(&ring));
        self.handle.send(UiUpdate::ShowLog(LogPane {
            title: container.name.clone(),
            source: ring,
        }));
        self.log_open = true;
        self.sync_feed();
    }

    fn sync_feed(&self) {
        self.feed.gate.set(self.log_open && self.visible.is_open());
    }

    fn set_engine(&mut self, running: bool) {
        if running == self.running {
            return;
        }
        if running {
            self.handle
                .send(UiUpdate::Page(Page::Message(screen("starting"))));
            let handle = self.handle.clone();
            // The real engine takes a moment; so does the demo.
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(1500));
                handle.send(UiUpdate::Page(Page::Tables));
            });
            self.running = true;
            self.send_chrome();
            self.send_tables();
        } else {
            self.running = false;
            self.handle.send(UiUpdate::CloseLog);
            self.send_state();
        }
    }

    fn name_of(&self, tab: usize, id: &str) -> String {
        if tab == 0 {
            self.containers
                .iter()
                .find(|container| container.id == id)
                .map_or_else(|| id.to_string(), |container| container.name.clone())
        } else {
            self.others[tab - 1]
                .iter()
                .find(|row| row.id == id)
                .map_or_else(|| id.to_string(), |row| row.cells[0].clone())
        }
    }

    fn ask_remove(&mut self, tab: usize, ids: Vec<String>) {
        let token = self.next_token;
        self.next_token += 1;
        let noun = ["containers", "images", "volumes", "networks"][tab.min(3)];
        let (title, accept) = if ids.len() == 1 {
            (
                format!("Remove {}?", self.name_of(tab, &ids[0])),
                "Remove".to_string(),
            )
        } else {
            (
                format!("Remove {} {noun}?", ids.len()),
                format!("Remove {}", ids.len()),
            )
        };
        self.handle.send(UiUpdate::Confirm(Confirm {
            token,
            title,
            message: "This cannot be undone.".into(),
            accept,
        }));
        self.pending.insert(token, (tab, ids));
    }

    fn remove(&mut self, tab: usize, ids: &[String]) {
        // One running container refuses, so the result shows a failure.
        let refused = (tab == 0 && ids.len() > 2)
            .then(|| {
                ids.iter()
                    .find(|id| self.containers.iter().any(|c| &c.id == *id && c.running))
                    .cloned()
            })
            .flatten();
        let removed: Vec<&String> = ids
            .iter()
            .filter(|id| Some(*id) != refused.as_ref())
            .collect();
        let names: Vec<String> = removed.iter().map(|id| self.name_of(tab, id)).collect();
        let refused_name = refused.as_ref().map(|id| self.name_of(tab, id));
        if tab == 0 {
            self.containers
                .retain(|container| !removed.contains(&&container.id));
        } else {
            self.others[tab - 1].retain(|row| !removed.contains(&&row.id));
        }
        self.send_tables();
        self.handle.send(UiUpdate::ClearSelection { tab });
        if ids.len() > 1 || refused_name.is_some() {
            let mut detail = format!("Removed: {}", names.join(", "));
            if let Some(name) = &refused_name {
                let _ = write!(detail, "\nFailed: {name}: container is running");
            }
            self.handle.send(UiUpdate::Notice(Notice {
                title: format!("Removed {} of {}", names.len(), ids.len()),
                message: if refused_name.is_some() {
                    "One could not be removed.".into()
                } else {
                    "All were removed.".into()
                },
                detail: Some(detail),
                tone: if refused_name.is_some() {
                    Tone::Warning
                } else {
                    Tone::Info
                },
            }));
        }
    }

    fn row_action(&mut self, id: &str, action: &str) {
        let Some(index) = self
            .containers
            .iter()
            .position(|container| container.id == id)
        else {
            return;
        };
        match action {
            "start" | "restart" => self.containers[index].running = true,
            "stop" => self.containers[index].running = false,
            "logs" => {
                self.open_log(index);
                return;
            }
            _ => return,
        }
        self.send_table(0);
    }

    /// Answers one event. Runs on the UI thread, so it only queues updates.
    pub fn on_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::WindowShown => {
                self.visible.set(true);
                // The window drops its lists when it closes; send them again.
                self.send_tables();
                self.sync_feed();
            }
            UiEvent::WindowHidden => {
                self.visible.set(false);
                self.sync_feed();
            }
            UiEvent::RowAction { tab: 0, id, action } => self.row_action(&id, &action),
            UiEvent::RemoveRequested { tab, ids } => self.ask_remove(tab, ids),
            UiEvent::Confirmed { token, accepted } => {
                if let Some((tab, ids)) = self.pending.remove(&token)
                    && accepted
                {
                    self.remove(tab, &ids);
                }
            }
            UiEvent::Button { key } => match key.as_str() {
                "start" | "retry" | "recheck" | "install" => self.set_engine(true),
                "stop" => self.set_engine(false),
                _ => {}
            },
            UiEvent::Tray(TrayCommand::StartEngine) => self.set_engine(true),
            UiEvent::Tray(TrayCommand::StopEngine) => self.set_engine(false),
            UiEvent::LogClosed => {
                self.log_open = false;
                self.sync_feed();
                *lock(&self.feed.ring) = None;
            }
            UiEvent::Refresh => self.send_tables(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_tables_have_a_cell_per_column() {
        let containers = containers();
        assert_eq!(containers.len(), 240);
        let tables = [
            container_table(&containers),
            simple_table(vec![Column::new("a", 1); 5], &images(), "images"),
            simple_table(vec![Column::new("a", 1); 3], &volumes(), "volumes"),
            simple_table(vec![Column::new("a", 1); 3], &networks(), "networks"),
        ];
        for table in &tables {
            assert!(
                table
                    .rows
                    .iter()
                    .all(|row| row.cells.len() == table.columns.len())
            );
            let mut ids: Vec<&str> = table.rows.iter().map(|row| row.id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), table.rows.len(), "ids are unique");
        }
    }

    #[test]
    fn meters_stay_in_range() {
        for tick in 0..200 {
            for meter in meters(tick) {
                if let MeterValue::Known { fill, .. } = meter.value {
                    assert!(
                        (0.0..=1.0).contains(&fill),
                        "{} at {tick}: {fill}",
                        meter.label
                    );
                }
            }
        }
    }

    #[test]
    fn fake_lines_carry_colour_and_hangul() {
        let mut line = String::new();
        fake_line(&mut line, 0);
        assert!(line.contains("\x1b[1;31mERROR"));
        assert!(line.ends_with("한글 로그"));
    }
}
