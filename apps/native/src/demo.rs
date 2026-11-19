//! A made-up engine, for looking at the window and measuring it without
//! Docker: four lists of a few hundred rows, usage figures that change every
//! second, and a log of 10,000 lines a second.
//!
//! It stands where `docklet_app::App` stands: the same controller drives the
//! same window, and what it asks for is answered from the data here with the
//! same messages. Nothing in it touches WSL, Docker or the engine pipe.
//!
//! The figures and the log run on their own threads and block on a
//! condition variable while nobody wants them: the figures run while the
//! window is shown, the log for as long as it is followed, which a
//! minimised window does not end and a closed one does.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use docklet_app::control::{EngineStatus, Transition};
use docklet_app::pull::{ReferenceError, check_reference};
use docklet_app::removal::{PruneTarget, RemovalPlan};
use docklet_app::{
    ContainerAction, ContainerUsage, DiskUsage, Engine, EngineState, HostReading, List, Message,
    PruneReport, RemoveReport, ResourceKind, SharedLog, Sink, WindowState,
};
use docklet_engine::models::{
    ContainerSummary, ContainerSummaryStateEnum, ImageSummary, Network, PortSummary,
    PortSummaryTypeEnum, Volume,
};
use docklet_engine::{DiskTotals, RemoveFailure, RemoveResult};
use docklet_wsl::usage::{DiskSpace, DockletProcesses, HostUsage, VmStatus, VmUsage};

pub const LINES_PER_SECOND: u64 = 10_000;
/// How often the log generator wakes. Windows may stretch it to its 15.6 ms
/// tick, so the batch size comes from the clock.
const TICK: Duration = Duration::from_millis(10);
/// Most lines one wake-up adds, so a thread that was held up does not catch
/// up in one burst.
const MAX_BURST: u64 = 2_000;

#[derive(Clone, Debug, Default)]
pub struct DemoOptions {
    /// Start in the state of one message screen instead of running: wsl,
    /// install, installing, stopped, sleeping, starting or error.
    pub screen: Option<String>,
    /// How many lines a second a followed log prints, instead of
    /// [`LINES_PER_SECOND`]: 20 is a container that talks now and then.
    pub lines_per_second: Option<u64>,
}

/// How many lines are due `elapsed` after a log started at `rate` lines a
/// second.
fn lines_due(elapsed: Duration, rate: u64) -> u64 {
    u64::try_from(elapsed.as_millis().saturating_mul(u128::from(rate)) / 1000).unwrap_or(u64::MAX)
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
    fn new() -> Self {
        Self {
            open: Mutex::new(false),
            wake: Condvar::new(),
        }
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
    project: Option<&'static str>,
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
const BASES: [&str; 3] = ["alpine", "debian-13", "ubuntu-26.04"];
const MB: u64 = 1_000_000;

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
            // Lazily: the last forty are past the end of the projects.
            project: grouped.then(|| PROJECTS[n / 25]),
            running: n % 3 != 0,
            port: (service < 2).then_some(8000 + n as u16),
            cpu: (n * 37 % 400) as u32,
            memory: (20 + n * 53 % 900) as u32,
        });
    }
    list
}

fn container_summary(container: &Container) -> ContainerSummary {
    let labels = container.project.map(|project| {
        let label = "com.docker.compose.project".to_string();
        HashMap::from([(label, project.to_string())])
    });
    let ports = container.port.map(|port| {
        vec![PortSummary {
            ip: None,
            private_port: 80,
            public_port: Some(port),
            typ: Some(PortSummaryTypeEnum::TCP),
        }]
    });
    let (state, status) = if container.running {
        (ContainerSummaryStateEnum::RUNNING, "Up 2 hours")
    } else {
        (ContainerSummaryStateEnum::EXITED, "Exited (0) 3 hours ago")
    };
    ContainerSummary {
        id: Some(container.id.clone()),
        names: Some(vec![format!("/{}", container.name)]),
        image: Some(container.image.to_string()),
        state: Some(state),
        status: Some(status.to_string()),
        ports,
        labels,
        ..Default::default()
    }
}

fn container_usage(container: &Container, tick: u64) -> ContainerUsage {
    // A figure that moves a little every second.
    let wobble = (tick * 7 + u64::from(container.cpu)) % 30;
    let memory = u64::from(container.memory) * MB;
    let limit = 8_000 * MB;
    ContainerUsage {
        id: container.id.clone(),
        name: container.name.clone(),
        cpu_percent: Some(f64::from(container.cpu) / 10.0 + wobble as f64 / 10.0),
        online_cpus: 8,
        memory_used: memory,
        memory_limit: limit,
        memory_percent: Some(memory as f64 / limit as f64 * 100.0),
        net_rx: (u64::from(container.cpu) + tick) * 12_000,
        net_tx: (u64::from(container.cpu) + tick) * 3_400,
        block_read: u64::from(container.memory) * 41_000,
        block_write: tick * 1_200,
        pids: 3,
    }
}

fn images() -> Vec<ImageSummary> {
    (0..60u64)
        .map(|n| {
            let repository = IMAGES[(n % 5) as usize].split(':').next().unwrap_or("");
            ImageSummary {
                id: format!(
                    "sha256:{:012x}",
                    0x51_7cc1_b727_u64.wrapping_mul(n + 3) & 0xffff_ffff_ffff
                ),
                repo_tags: vec![format!(
                    "registry.example.com/team/{repository}:v{}.{}",
                    n % 4 + 1,
                    n % 10
                )],
                size: ((40 + n * 137 % 1900) * MB) as i64,
                created: 1_794_000_000 - (n * 3 % 90 + 1) as i64 * 86_400,
                // Every third one is in use.
                containers: i64::from(n % 3 == 0),
                ..Default::default()
            }
        })
        .collect()
}

fn volumes() -> Vec<Volume> {
    (0..30usize)
        .map(|n| Volume {
            name: format!("{}_data{}", PROJECTS[n % 8], n / 8),
            driver: "local".to_string(),
            ..Default::default()
        })
        .collect()
}

fn networks() -> Vec<Network> {
    let network = |name: String, driver: &str| Network {
        id: Some(format!("net-{name}")),
        name: Some(name),
        driver: Some(driver.to_string()),
        scope: Some("local".to_string()),
        ..Default::default()
    };
    let mut list = vec![
        network("bridge".to_string(), "bridge"),
        network("host".to_string(), "host"),
        network("none".to_string(), "null"),
    ];
    list.extend(PROJECTS.map(|project| network(format!("{project}_default"), "bridge")));
    list
}

fn host_reading(tick: u64) -> HostReading {
    let wave = |period: u64, phase: u64| {
        let position = (tick + phase) % period;
        let half = period / 2;
        let rising = if position < half {
            position
        } else {
            period - position
        };
        rising as f64 / half as f64
    };
    let memory = ((1.2 + 5.6 * wave(40, 0)) * 1e9) as u64;
    HostReading::Sample(HostUsage {
        vm: VmStatus::Found(VmUsage {
            pid: 4321,
            name: "vmmemWSL".to_string(),
            working_set_bytes: memory + 300 * MB,
            private_working_set_bytes: memory,
            cpu_percent: Some(3.0 + 90.0 * wave(16, 5)),
        }),
        // No counters for this VM: the meter says so in words.
        gpu: None,
        disk: DiskSpace {
            vhdx_bytes: Some(18_200 * MB + tick % 600 * 10 * MB),
            free_bytes: Some(46_000 * MB),
        },
        docklet: DockletProcesses::default(),
    })
}

/// What the made-up engine holds.
struct State {
    engine: EngineState,
    window: WindowState,
    containers: Vec<Container>,
    images: Vec<ImageSummary>,
    volumes: Vec<Volume>,
    networks: Vec<Network>,
    /// The list the window shows.
    list: Option<ResourceKind>,
    usage: bool,
    /// The log that is followed, by its token.
    log: Option<(u32, SharedLog)>,
    next_token: u32,
    /// Steps of an install that never ends, shown once: `--screen installing`.
    stuck_install: bool,
}

impl State {
    fn list_of(&self, kind: ResourceKind) -> List {
        match kind {
            ResourceKind::Container => {
                List::Containers(self.containers.iter().map(container_summary).collect())
            }
            ResourceKind::Image => List::Images(self.images.clone()),
            ResourceKind::Volume => List::Volumes(self.volumes.clone()),
            ResourceKind::Network => List::Networks(self.networks.clone()),
        }
    }

    fn disk_usage(&self) -> DiskUsage {
        let images: u64 = self.images.iter().map(|image| image.size as u64).sum();
        let unused: u64 = self
            .images
            .iter()
            .filter(|image| image.containers == 0)
            .map(|image| image.size as u64)
            .sum();
        let in_use = self.images.iter().filter(|image| image.containers > 0);
        let volumes = self.volumes.len() as u64;
        DiskUsage {
            image_totals: DiskTotals {
                count: self.images.len() as u64,
                active: in_use.count() as u64,
                size: images,
                reclaimable: unused,
            },
            container_totals: DiskTotals {
                count: self.containers.len() as u64,
                active: self.containers.iter().filter(|c| c.running).count() as u64,
                size: self.containers.len() as u64 * 3 * MB,
                reclaimable: 0,
            },
            volume_totals: DiskTotals {
                count: volumes,
                active: volumes / 2,
                size: volumes * 210 * MB,
                reclaimable: volumes / 2 * 210 * MB,
            },
            ..Default::default()
        }
    }
}

struct Inner {
    sink: Arc<dyn Sink>,
    state: Mutex<State>,
    /// A start is in flight.
    acting: AtomicBool,
    /// Open while the window shows usage figures.
    figures: Gate,
    /// Open while a log is followed and the window shows it.
    logging: Gate,
    /// Lines a second a followed log prints.
    rate: u64,
}

impl Inner {
    fn post(&self, message: Message) {
        self.sink.post(message);
    }

    fn post_status(&self) {
        let state = lock(&self.state).engine.clone();
        self.post(Message::Status(Ok(EngineStatus {
            state,
            bases: BASES.to_vec(),
            mirrored_networking: false,
        })));
    }

    fn runs(&self) -> bool {
        lock(&self.state).engine == EngineState::Running
    }

    /// Opens and closes the gates of the two threads to match the state.
    /// A followed log goes on behind a minimised window, as a real one
    /// does; only a closed window ends it.
    fn sync_gates(&self) {
        let state = lock(&self.state);
        let visible = state.window == WindowState::Visible;
        self.figures.set(visible && state.usage);
        self.logging
            .set(state.window != WindowState::Gone && state.log.is_some());
    }

    /// Sends the list the window shows again, after it changed.
    fn post_list(&self, kind: ResourceKind) {
        let list = {
            let state = lock(&self.state);
            (state.list == Some(kind) && state.engine == EngineState::Running)
                .then(|| state.list_of(kind))
        };
        if let Some(list) = list {
            let result = Ok(list);
            self.post(Message::List { kind, result });
        }
    }

    /// The figures, once a second while the gate is open.
    fn sample(&self) {
        let mut tick = 0u64;
        loop {
            self.figures.wait();
            // The containers' figures only for the list that shows them, as
            // the real engine takes them.
            let usage: Option<Vec<ContainerUsage>> = {
                let state = lock(&self.state);
                let listed = state.list == Some(ResourceKind::Container);
                (state.engine == EngineState::Running && listed).then(|| {
                    let running = state.containers.iter().filter(|c| c.running);
                    running.map(|c| container_usage(c, tick)).collect()
                })
            };
            if tick.is_multiple_of(2) {
                self.post(Message::HostUsage(host_reading(tick)));
            }
            if usage.is_some() {
                self.post(Message::ContainerUsage(usage));
            }
            tick += 1;
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Log lines into the followed log, at the demo's rate while the gate
    /// is open.
    fn feed(&self) {
        let mut sequence = 0u64;
        loop {
            self.logging.wait();
            let started = Instant::now();
            let mut accounted = 0u64;
            while self.logging.is_open() {
                let due = lines_due(started.elapsed(), self.rate);
                let batch = (due - accounted).min(MAX_BURST);
                accounted = due;
                let log = lock(&self.state).log.as_ref().map(|(_, log)| log.clone());
                // A tick on which no line is due adds nothing, as a real
                // log that is quiet sends nothing.
                if let Some(log) = log.filter(|_| batch > 0) {
                    log.append((0..batch).map(|_| {
                        let mut line = String::with_capacity(128);
                        fake_line(&mut line, sequence);
                        sequence += 1;
                        line
                    }));
                }
                thread::sleep(TICK);
            }
        }
    }
}

/// The made-up engine. Cheap to clone; every clone is the same engine.
#[derive(Clone)]
pub struct Demo {
    inner: Arc<Inner>,
}

impl Demo {
    /// A made-up engine that answers through `sink`, and its two threads.
    pub fn start(sink: Arc<dyn Sink>, options: &DemoOptions) -> Self {
        let engine = match options.screen.as_deref() {
            None => EngineState::Running,
            Some("wsl") => EngineState::WslMissing,
            Some("install" | "installing") => EngineState::NotInstalled,
            Some("sleeping") => EngineState::Sleeping,
            Some("starting") => EngineState::Starting,
            Some("error") => EngineState::Error(
                "dockerd exited with status 1:\nfailed to start daemon: pid file found, ensure \
                 docker is not running or delete /var/run/docker.pid"
                    .to_string(),
            ),
            Some(_) => EngineState::Stopped,
        };
        let inner = Arc::new(Inner {
            sink,
            state: Mutex::new(State {
                engine,
                window: WindowState::Gone,
                containers: containers(),
                images: images(),
                volumes: volumes(),
                networks: networks(),
                list: None,
                usage: false,
                log: None,
                next_token: 1,
                stuck_install: options.screen.as_deref() == Some("installing"),
            }),
            acting: AtomicBool::new(false),
            figures: Gate::new(),
            logging: Gate::new(),
            rate: options.lines_per_second.unwrap_or(LINES_PER_SECOND),
        });
        let sampler = inner.clone();
        thread::spawn(move || sampler.sample());
        let feeder = inner.clone();
        thread::spawn(move || feeder.feed());
        Self { inner }
    }

    /// Does `work` on a thread of its own, as the real engine's requests
    /// take their time.
    fn later(&self, work: impl FnOnce(&Inner) + Send + 'static) {
        let inner = self.inner.clone();
        thread::spawn(move || work(&inner));
    }
}

impl Engine for Demo {
    fn set_window(&self, window: WindowState) {
        let stuck = {
            let mut state = lock(&self.inner.state);
            state.window = window;
            // As `docklet_app::App` does: no list for a window nobody sees,
            // and no log for one that is closed.
            if window != WindowState::Visible {
                state.list = None;
            }
            if window == WindowState::Gone {
                state.log = None;
            }
            window == WindowState::Visible && std::mem::take(&mut state.stuck_install)
        };
        self.inner.sync_gates();
        if window == WindowState::Visible {
            self.inner.post_status();
        }
        if stuck {
            for step in ["Downloading the rootfs", "Importing the distro"] {
                self.inner.post(Message::InstallProgress(step.to_string()));
            }
        }
    }

    fn is_acting(&self) -> bool {
        self.inner.acting.load(Ordering::Relaxed)
    }

    fn refresh_status(&self) {
        self.inner.post_status();
    }

    fn install(&self, base: Option<String>) {
        self.later(move |inner| {
            let base = base.unwrap_or_else(|| BASES[0].to_string());
            let steps = [
                format!("Downloading the rootfs for {base}"),
                "Checking its checksum".to_string(),
                "Importing the distro".to_string(),
                "Starting dockerd".to_string(),
            ];
            for step in steps {
                inner.post(Message::InstallProgress(step));
                thread::sleep(Duration::from_millis(500));
            }
            lock(&inner.state).engine = EngineState::Stopped;
            inner.post(Message::EngineRequestDone {
                transition: Transition::Installing,
                error: None,
            });
            inner.post_status();
        });
    }

    fn start(&self) {
        self.inner.acting.store(true, Ordering::Relaxed);
        self.later(|inner| {
            // The real engine takes a moment; so does this one.
            thread::sleep(Duration::from_millis(1500));
            lock(&inner.state).engine = EngineState::Running;
            inner.acting.store(false, Ordering::Relaxed);
            inner.post(Message::EngineRequestDone {
                transition: Transition::Starting,
                error: None,
            });
            inner.post_status();
        });
    }

    fn stop(&self) {
        {
            let mut state = lock(&self.inner.state);
            state.engine = EngineState::Stopped;
            state.log = None;
        }
        self.inner.sync_gates();
        self.inner.post(Message::EngineRequestDone {
            transition: Transition::Stopping,
            error: None,
        });
        self.inner.post_status();
    }

    fn show_list(&self, kind: Option<ResourceKind>) {
        lock(&self.inner.state).list = kind;
        if let Some(kind) = kind {
            self.reload(kind);
        }
    }

    fn reload(&self, kind: ResourceKind) {
        if self.inner.runs() {
            let result = Ok(lock(&self.inner.state).list_of(kind));
            self.inner.post(Message::List { kind, result });
        }
    }

    fn container_action(&self, id: String, action: ContainerAction) {
        self.later(move |inner| {
            thread::sleep(Duration::from_millis(400));
            {
                let mut state = lock(&inner.state);
                let found = state.containers.iter().position(|c| c.id == id);
                match (found, action) {
                    (Some(index), ContainerAction::Start | ContainerAction::Restart) => {
                        state.containers[index].running = true;
                    }
                    (Some(index), ContainerAction::Stop) => {
                        state.containers[index].running = false;
                    }
                    (Some(index), ContainerAction::Remove) => {
                        state.containers.remove(index);
                    }
                    (None, _) => {}
                }
            }
            let error = None;
            inner.post(Message::ActionDone { id, action, error });
            inner.post_list(ResourceKind::Container);
        });
    }

    fn remove(&self, plan: RemovalPlan) {
        let kind = plan.kind;
        let report = {
            let mut state = lock(&self.inner.state);
            // One of a bigger removal refuses, so the result shows a failure:
            // a running container, or an image or volume that is in use.
            let refused = (plan.items.len() > 2)
                .then(|| plan.items.iter().find(|item| item.running || item.in_use))
                .flatten()
                .map(|item| item.id.clone());
            let results = plan.items.iter().map(|item| {
                if Some(&item.id) == refused.as_ref() {
                    let said = "it is in use (says the demo)";
                    return RemoveResult::failed(
                        kind,
                        item.id.as_str(),
                        RemoveFailure::InUse,
                        said,
                    );
                }
                RemoveResult::removed(kind, item.id.as_str())
            });
            let report = RemoveReport {
                results: results.collect(),
            };
            let removed = |id: &str| {
                let result = report.results.iter().find(|result| result.id == id);
                result.is_some_and(RemoveResult::is_removed)
            };
            match kind {
                ResourceKind::Container => state.containers.retain(|c| !removed(&c.id)),
                ResourceKind::Image => state.images.retain(|image| !removed(&image.id)),
                ResourceKind::Volume => state.volumes.retain(|volume| !removed(&volume.name)),
                ResourceKind::Network => state
                    .networks
                    .retain(|network| !network.id.as_deref().is_some_and(removed)),
            }
            report
        };
        self.inner.post(Message::Removed { plan, report });
        self.inner.post_list(kind);
    }

    fn prune(&self, target: PruneTarget) {
        let report = {
            let mut state = lock(&self.inner.state);
            let mut report = PruneReport::default();
            match target {
                PruneTarget::StoppedContainers => {
                    let stopped = state.containers.iter().filter(|c| !c.running);
                    report.deleted = stopped.map(|c| c.id.clone()).collect();
                    report.space_reclaimed = report.deleted.len() as u64 * 3 * MB;
                    state.containers.retain(|c| c.running);
                }
                // None of the made-up images is untagged, and none of the
                // volumes anonymous: nothing to remove.
                PruneTarget::DanglingImages | PruneTarget::AnonymousVolumes => {}
                PruneTarget::UnusedImages => {
                    let unused = state.images.iter().filter(|image| image.containers == 0);
                    report.space_reclaimed = unused.clone().map(|image| image.size as u64).sum();
                    report.deleted = unused.map(|image| image.id.clone()).collect();
                    state.images.retain(|image| image.containers > 0);
                }
                PruneTarget::UnusedVolumes => {
                    let keep = state.volumes.len() / 2;
                    let unused = state.volumes.split_off(keep);
                    report.deleted = unused.into_iter().map(|volume| volume.name).collect();
                    report.space_reclaimed = report.deleted.len() as u64 * 210 * MB;
                }
                PruneTarget::UnusedNetworks => {
                    let keep = state.networks.len().saturating_sub(2);
                    let unused = state.networks.split_off(keep);
                    report.deleted = unused.into_iter().filter_map(|n| n.name).collect();
                }
            }
            report
        };
        let result = Ok(report);
        self.inner.post(Message::Pruned { target, result });
        self.inner.post_list(target.kind());
    }

    fn pull(&self, typed: &str) {
        let reference = match check_reference(typed) {
            Ok(reference) => reference,
            Err(ReferenceError::Empty) => return,
            Err(ReferenceError::Invalid(error)) => {
                let reference = typed.trim().to_string();
                let error = Some(error);
                return self.inner.post(Message::PullDone { reference, error });
            }
        };
        self.later(move |inner| {
            for layer in ["a3ed95caeb02", "5f70bf18a086", "c1c8a4e3b2d0"] {
                for percent in [0, 35, 70, 100] {
                    let line = format!("{layer} Downloading {percent}%");
                    inner.post(Message::PullProgress(line));
                    thread::sleep(Duration::from_millis(120));
                }
                inner.post(Message::PullProgress(format!("{layer} Pull complete")));
            }
            {
                let mut state = lock(&inner.state);
                let number = state.images.len() as u64 + 100;
                state.images.insert(
                    0,
                    ImageSummary {
                        id: format!("sha256:{number:012x}"),
                        repo_tags: vec![reference.clone()],
                        size: (187 * MB) as i64,
                        created: 1_794_000_000,
                        ..Default::default()
                    },
                );
            }
            let error = None;
            inner.post(Message::PullDone { reference, error });
            inner.post_list(ResourceKind::Image);
        });
    }

    fn follow_logs(&self, _id: String, log: SharedLog) -> u32 {
        let token = {
            let mut state = lock(&self.inner.state);
            let token = state.next_token;
            state.next_token += 1;
            state.log = Some((token, log));
            token
        };
        self.inner.sync_gates();
        token
    }

    fn stop_logs(&self, token: u32) {
        {
            let mut state = lock(&self.inner.state);
            if state.log.as_ref().is_some_and(|(open, _)| *open == token) {
                state.log = None;
            }
        }
        self.inner.sync_gates();
    }

    fn set_usage(&self, shown: bool) {
        lock(&self.inner.state).usage = shown;
        self.inner.sync_gates();
    }

    fn load_disk_usage(&self) {
        self.later(|inner| {
            // Docker takes seconds over this on a big engine.
            thread::sleep(Duration::from_millis(800));
            let usage = lock(&inner.state).disk_usage();
            inner.post(Message::DiskUsage(Ok(usage)));
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use docklet_app::table::{containers_table, images_table, networks_table, volumes_table};

    use super::*;

    /// A made-up engine and what it posts. Its threads stay blocked: the
    /// window is never said to be visible with usage or a log.
    fn demo(screen: Option<&str>) -> (Demo, Receiver<Message>) {
        let (sender, messages) = mpsc::channel();
        let sink = move |message: Message| {
            let _ = sender.send(message);
        };
        let options = DemoOptions {
            screen: screen.map(str::to_string),
            lines_per_second: None,
        };
        (Demo::start(Arc::new(sink), &options), messages)
    }

    #[test]
    fn a_slow_log_has_ticks_on_which_no_line_is_due() {
        // 20 lines a second: one every 50 ms, none in between.
        assert_eq!(lines_due(Duration::from_millis(49), 20), 0);
        assert_eq!(lines_due(Duration::from_millis(50), 20), 1);
        assert_eq!(lines_due(Duration::from_secs(5), 20), 100);
        assert_eq!(lines_due(Duration::from_millis(10), LINES_PER_SECOND), 100);
        assert_eq!(lines_due(Duration::from_secs(3), 0), 0);
    }

    #[test]
    fn the_fake_lists_become_tables_with_unique_ids() {
        let state = State {
            engine: EngineState::Running,
            window: WindowState::Gone,
            containers: containers(),
            images: images(),
            volumes: volumes(),
            networks: networks(),
            list: None,
            usage: false,
            log: None,
            next_token: 1,
            stuck_install: false,
        };
        assert_eq!(state.containers.len(), 240);
        let summaries: Vec<ContainerSummary> =
            state.containers.iter().map(container_summary).collect();
        let tables = [
            containers_table(&summaries, None),
            images_table(&state.images),
            volumes_table(&state.volumes, None),
            networks_table(&state.networks),
        ];
        for table in &tables {
            assert!(!table.rows.is_empty());
            let mut ids: Vec<&str> = table.rows.iter().map(|row| row.id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), table.rows.len(), "ids are unique");
        }
        // Eight Compose projects, and forty containers outside any.
        let grouped = tables[0].rows.iter().filter(|row| row.group.is_some());
        assert_eq!(grouped.count(), 200);
        let usage = state.disk_usage();
        assert!(usage.image_totals.reclaimable < usage.image_totals.size);
    }

    #[test]
    fn the_figures_stay_in_range() {
        for tick in 0..200 {
            let HostReading::Sample(usage) = host_reading(tick) else {
                panic!("a sample");
            };
            let VmStatus::Found(vm) = usage.vm else {
                panic!("a VM");
            };
            let cpu = vm.cpu_percent.unwrap();
            assert!((0.0..=100.0).contains(&cpu), "cpu at {tick}: {cpu}");
            assert!(vm.private_working_set_bytes < 8_000 * MB);
        }
        let container = &containers()[1];
        let usage = container_usage(container, 3);
        assert_eq!(usage.id, container.id);
        assert!(usage.cpu_percent.unwrap() >= 0.0);
    }

    #[test]
    fn fake_lines_carry_colour_and_hangul() {
        let mut line = String::new();
        fake_line(&mut line, 0);
        assert!(line.contains("\x1b[1;31mERROR"));
        assert!(line.ends_with("한글 로그"));
    }

    #[test]
    fn the_engine_starts_in_the_state_of_the_screen_asked_for() {
        let state = |screen: Option<&str>| {
            let (demo, messages) = demo(screen);
            demo.refresh_status();
            match messages.recv().unwrap() {
                Message::Status(Ok(status)) => status.state,
                other => panic!("expected a status, got {other:?}"),
            }
        };
        assert_eq!(state(None), EngineState::Running);
        assert_eq!(state(Some("wsl")), EngineState::WslMissing);
        assert_eq!(state(Some("install")), EngineState::NotInstalled);
        assert_eq!(state(Some("sleeping")), EngineState::Sleeping);
        assert_eq!(state(Some("stopped")), EngineState::Stopped);
        assert!(matches!(state(Some("error")), EngineState::Error(_)));
    }

    #[test]
    fn a_list_is_only_sent_from_a_running_engine() {
        let (demo, messages) = demo(Some("sleeping"));
        demo.show_list(Some(ResourceKind::Image));
        assert!(messages.try_recv().is_err());

        let (demo, messages) = demo_running();
        demo.show_list(Some(ResourceKind::Image));
        match messages.recv().unwrap() {
            Message::List {
                kind: ResourceKind::Image,
                result: Ok(List::Images(images)),
            } => assert_eq!(images.len(), 60),
            other => panic!("expected the images, got {other:?}"),
        }
    }

    fn demo_running() -> (Demo, Receiver<Message>) {
        demo(None)
    }

    #[test]
    fn a_big_removal_leaves_one_behind_and_says_so() {
        let (demo, messages) = demo_running();
        let table = networks_table(&lock(&demo.inner.state).networks);
        let plan = RemovalPlan::new(ResourceKind::Network, &table.rows);
        // The three predefined networks are not in the plan at all.
        assert_eq!(plan.items.len(), 8);
        demo.remove(plan);
        match messages.recv().unwrap() {
            Message::Removed { report, .. } => {
                // Networks are never "in use" here: all eight go.
                assert_eq!(report.removed(), 8);
            }
            other => panic!("expected the removal's end, got {other:?}"),
        }
        assert_eq!(lock(&demo.inner.state).networks.len(), 3);

        let summaries: Vec<ContainerSummary> = {
            let state = lock(&demo.inner.state);
            state.containers.iter().map(container_summary).collect()
        };
        let table = containers_table(&summaries, None);
        let plan = RemovalPlan::new(ResourceKind::Container, &table.rows[..5]);
        demo.remove(plan);
        match messages.recv().unwrap() {
            Message::Removed { report, .. } => {
                assert_eq!((report.removed(), report.failed()), (4, 1));
            }
            other => panic!("expected the removal's end, got {other:?}"),
        }
        assert_eq!(lock(&demo.inner.state).containers.len(), 236);
    }

    #[test]
    fn a_prune_removes_what_it_names_and_reports_the_space() {
        let (demo, messages) = demo_running();
        demo.prune(PruneTarget::StoppedContainers);
        match messages.recv().unwrap() {
            Message::Pruned {
                result: Ok(report), ..
            } => {
                assert_eq!(report.deleted.len(), 80);
                assert_eq!(report.space_reclaimed, 240 * MB);
            }
            other => panic!("expected the prune's end, got {other:?}"),
        }
        assert!(lock(&demo.inner.state).containers.iter().all(|c| c.running));
        demo.prune(PruneTarget::DanglingImages);
        match messages.recv().unwrap() {
            Message::Pruned {
                result: Ok(report), ..
            } => assert!(report.deleted.is_empty()),
            other => panic!("expected the prune's end, got {other:?}"),
        }
    }

    #[test]
    fn a_followed_log_is_let_go_with_its_token_or_with_the_window() {
        let (demo, _messages) = demo_running();
        let token = demo.follow_logs("x".to_string(), SharedLog::new());
        let followed = |demo: &Demo| lock(&demo.inner.state).log.is_some();
        assert!(followed(&demo));
        demo.stop_logs(token + 1);
        assert!(followed(&demo));
        demo.stop_logs(token);
        assert!(!followed(&demo));
        demo.follow_logs("x".to_string(), SharedLog::new());
        demo.set_window(WindowState::Gone);
        assert!(!followed(&demo));
        // Nothing runs for a window that is away.
        assert!(!demo.inner.logging.is_open() && !demo.inner.figures.is_open());
    }

    #[test]
    fn a_minimised_window_keeps_its_log_and_nothing_else() {
        let (demo, _messages) = demo_running();
        demo.show_list(Some(ResourceKind::Container));
        demo.set_usage(true);
        // No log is followed: minimising leaves no thread with work.
        demo.set_window(WindowState::Hidden);
        let state = lock(&demo.inner.state);
        assert_eq!(state.list, None);
        drop(state);
        assert!(!demo.inner.logging.is_open() && !demo.inner.figures.is_open());
        // A log that is followed stays so, and is let go of when the
        // window closes. (The window was never visible here, so the log's
        // thread found its gate shut.)
        let (demo, _messages) = demo_running();
        lock(&demo.inner.state).log = Some((1, SharedLog::new()));
        lock(&demo.inner.state).window = WindowState::Hidden;
        demo.set_window(WindowState::Gone);
        assert!(lock(&demo.inner.state).log.is_none());
    }

    #[test]
    fn what_was_typed_is_checked_before_anything_is_pulled() {
        let (demo, messages) = demo_running();
        demo.pull("  ");
        assert!(messages.try_recv().is_err());
        demo.pull("nginx:");
        match messages.recv().unwrap() {
            Message::PullDone { reference, error } => {
                assert_eq!(reference, "nginx:");
                assert!(error.is_some());
            }
            other => panic!("expected the pull's end, got {other:?}"),
        }
    }
}
