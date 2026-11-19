//! When to stop an idle engine and when to start it again, as pure logic.
//!
//! The pipe proxy sees every Docker connection, so it decides. It feeds
//! events into an `IdleMachine` (a connection opened, carried data or closed,
//! an engine call finished, a deadline passed) and carries out the
//! `IdleAction`s it gets back. The machine never blocks and never reads a
//! clock: every call takes `now`, so the tests drive it with a fake clock.
//!
//! The engine is stopped when, for `IdlePolicy::idle_after`, no connection
//! carried a byte, opened or closed, and no check saw a running container.
//! A connection that is open but quiet does not count: Docker Desktop, an
//! IDE or a forgotten `docker events` keep a connection open for as long as
//! the engine is up, and only ever receive. With [`Watchers::KeepAwake`]
//! every open connection counts instead, as it did before.
//!
//! Checks run only while the idle clock runs, at most once per
//! `IdlePolicy::check_every`. While the distro is stopped nothing is
//! scheduled at all; the next connection starts it.
//!
//! A watcher whose connection an idle stop closed tends to reconnect within
//! seconds, and waking the engine for that would undo the stop. So the
//! machine remembers the client processes of the connections the stop
//! closed. While the engine is stopped, a new connection from one of them
//! whose first request only subscribes or asks whether the engine is there
//! ([`FirstRequest::Passive`]) is held: it is neither relayed nor allowed to
//! wake the engine, and it is relayed once something else wakes it. Any
//! other request, and any connection from another process, wakes the engine
//! as before.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::doctor::Level;

/// Blocking engine calls the proxy needs. Each one may run `wsl.exe` and take
/// seconds (a start takes up to two minutes), so the proxy never calls them on
/// its accept loop.
pub trait EngineHooks: Send + Sync {
    /// Starts the distro, holds it up and waits for dockerd. Succeeds at once
    /// if the engine already runs.
    fn start_engine(&self) -> io::Result<()>;
    /// Stops the distro, as `docklet stop` does.
    fn stop_distro(&self) -> io::Result<()>;
    /// Whether the distro is running. Must not start it.
    fn distro_running(&self) -> io::Result<bool>;
    /// How many containers are running. Only called right after
    /// `distro_running` returned true, since asking dockerd would otherwise
    /// boot the distro.
    fn running_containers(&self) -> io::Result<usize>;
}

impl<H: EngineHooks + ?Sized> EngineHooks for std::sync::Arc<H> {
    fn start_engine(&self) -> io::Result<()> {
        (**self).start_engine()
    }
    fn stop_distro(&self) -> io::Result<()> {
        (**self).stop_distro()
    }
    fn distro_running(&self) -> io::Result<bool> {
        (**self).distro_running()
    }
    fn running_containers(&self) -> io::Result<usize> {
        (**self).running_containers()
    }
}

/// What one idle check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// The distro is not running.
    DistroStopped,
    /// The distro runs, with this many running containers.
    Containers(usize),
    /// The check failed. Counts as in use: nothing is stopped on a guess.
    Unknown,
}

/// Runs one idle check with `hooks`: the distro first, so that a stopped
/// distro is never booted just to ask dockerd.
pub fn check_activity<H: EngineHooks + ?Sized>(hooks: &H) -> Activity {
    match hooks.distro_running() {
        Ok(false) => Activity::DistroStopped,
        Ok(true) => match hooks.running_containers() {
            Ok(count) => Activity::Containers(count),
            Err(_) => Activity::Unknown,
        },
        Err(_) => Activity::Unknown,
    }
}

/// Whether a connection that carries nothing keeps the engine awake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Watchers {
    /// Only connections that carry data count as use. A quiet one is closed
    /// when the engine is stopped, and its client cannot wake the engine
    /// again by only subscribing.
    #[default]
    Ignored,
    /// Every open connection counts as use, however quiet.
    KeepAwake,
}

impl Watchers {
    /// Parses the value of the `watchers-keep-awake` setting: `yes` or `no`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("yes") {
            Some(Self::KeepAwake)
        } else if value.eq_ignore_ascii_case("no") {
            Some(Self::Ignored)
        } else {
            None
        }
    }

    /// The setting's value: `yes` when watchers keep the engine awake.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignored => "no",
            Self::KeepAwake => "yes",
        }
    }
}

impl fmt::Display for Watchers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How long the engine may sit unused before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdlePolicy {
    idle_after: Duration,
    check_every: Duration,
    watchers: Watchers,
}

impl IdlePolicy {
    /// Shortest allowed time between two checks. Each check runs `wsl.exe`
    /// twice; a smaller value would only burn CPU.
    pub const MIN_CHECK_EVERY: Duration = Duration::from_secs(1);
    /// Longest time between two checks unless the caller asks for more.
    pub const DEFAULT_CHECK_EVERY: Duration = Duration::from_secs(60);
    /// Upper bound for both durations, which keeps `Instant` arithmetic
    /// from overflowing.
    const MAX: Duration = Duration::from_secs(365 * 24 * 60 * 60);

    /// Stops the engine after `idle_after` without traffic or running
    /// containers. Checks run every `idle_after`, but at least once a minute
    /// and at most once a second. Quiet connections do not count.
    pub fn new(idle_after: Duration) -> Self {
        let idle_after = idle_after.min(Self::MAX);
        Self {
            idle_after,
            check_every: idle_after.clamp(Self::MIN_CHECK_EVERY, Self::DEFAULT_CHECK_EVERY),
            watchers: Watchers::Ignored,
        }
    }

    /// Sets the time between two checks, at least `MIN_CHECK_EVERY`.
    pub fn with_check_every(mut self, every: Duration) -> Self {
        self.check_every = every.clamp(Self::MIN_CHECK_EVERY, Self::MAX);
        self
    }

    /// Sets whether quiet connections keep the engine awake.
    pub fn with_watchers(mut self, watchers: Watchers) -> Self {
        self.watchers = watchers;
        self
    }

    pub fn idle_after(&self) -> Duration {
        self.idle_after
    }

    pub fn check_every(&self) -> Duration {
        self.check_every
    }

    pub fn watchers(&self) -> Watchers {
        self.watchers
    }
}

/// Identity of a client process: its ID and when it started, so that an ID
/// the system handed to a new process is not taken for the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId {
    pub pid: u32,
    /// Creation time as the system reports it; 0 when it could not be read.
    pub started: u64,
}

/// What the first request of a connection asks for, as far as waking the
/// engine for it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRequest {
    /// Nothing read yet, or the request line is not complete.
    Unread,
    /// Subscribes, or asks whether the engine is there: `/_ping`,
    /// `/version`, `/info`, `/events`, and `/grpc`, the BuildKit channel
    /// that build-history watchers such as Docker Desktop listen on.
    Passive,
    /// Anything else, including bytes that are not an HTTP request line.
    Active,
}

/// A request line longer than this is not one the classification knows.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;

/// Classifies the first bytes a client sent. Only the request line counts.
pub fn classify_request(head: &[u8]) -> FirstRequest {
    let Some(end) = head.iter().position(|byte| *byte == b'\n') else {
        return if head.len() >= MAX_REQUEST_LINE {
            FirstRequest::Active
        } else {
            FirstRequest::Unread
        };
    };
    let line = head[..end].strip_suffix(b"\r").unwrap_or(&head[..end]);
    let Ok(line) = std::str::from_utf8(line) else {
        return FirstRequest::Active;
    };
    let mut words = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return FirstRequest::Active;
    };
    if !version.starts_with("HTTP/") {
        return FirstRequest::Active;
    }
    let path = target.split('?').next().unwrap_or_default();
    let passive = matches!(
        (method, without_api_version(path)),
        ("GET" | "HEAD", "/_ping") | ("GET", "/version" | "/info" | "/events") | ("POST", "/grpc")
    );
    if passive {
        FirstRequest::Passive
    } else {
        FirstRequest::Active
    }
}

/// `/v1.47/events` is `/events`.
fn without_api_version(path: &str) -> &str {
    let Some(rest) = path.strip_prefix("/v") else {
        return path;
    };
    match rest.find('/') {
        Some(slash)
            if slash > 0
                && rest[..slash]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.') =>
        {
            &rest[slash..]
        }
        _ => path,
    }
}

/// What the machine believes about the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePhase {
    /// Not known yet, or a start or stop failed. A connection starts the
    /// engine (starting a running engine is cheap); idle checks find out.
    Unknown,
    Stopped,
    Starting,
    Running,
    Stopping,
}

impl EnginePhase {
    const ALL: [Self; 5] = [
        Self::Unknown,
        Self::Stopped,
        Self::Starting,
        Self::Running,
        Self::Stopping,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.as_str() == word)
    }
}

/// A blocking engine call the driver must run, then report back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    /// Call `EngineHooks::start_engine`, then `start_finished`.
    Start,
    /// Call `EngineHooks::stop_distro`, then `stop_finished`. On success the
    /// driver closes every connection that was open when the stop began.
    Stop,
    /// Call `check_activity`, then `check_finished`.
    Check,
}

/// What a new connection has to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The engine runs: relay now.
    Proceed,
    /// Wait for the start in flight; `start_finished` says how it went.
    Wait,
    /// A watcher the last idle stop cut off, asking for nothing that needs
    /// the engine: keep the connection open without relaying it, and ask
    /// again when the engine has started or the client sent more. The
    /// machine does not count it.
    Hold,
}

#[derive(Debug, Clone, Copy)]
struct Connection {
    client: Option<ClientId>,
    /// Admitted with `Admission::Wait`, and the start has not finished.
    waiting: bool,
    last_traffic: Instant,
}

/// A connection the machine counts, as the proxy's report shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionView {
    pub id: u64,
    pub client: Option<ClientId>,
    pub waiting: bool,
    pub last_traffic: Instant,
}

/// Idle stop and wake on connect, without any I/O. See the module docs.
#[derive(Debug)]
pub struct IdleMachine {
    policy: IdlePolicy,
    phase: EnginePhase,
    /// Admitted connections by the driver's ID, until `disconnect`.
    connections: BTreeMap<u64, Connection>,
    /// Clients of the quiet connections the last idle stop closed. Cleared
    /// whenever a start begins.
    watchers: Vec<ClientId>,
    /// Since when the engine has been unused. Meaningful only while it can
    /// be idle (see `idle_clock_runs`).
    idle_since: Instant,
    next_check: Instant,
    last_check: Option<Instant>,
    checking: bool,
    /// A connection came, went or carried data while the check ran, so its
    /// result may no longer hold.
    check_stale: bool,
}

impl IdleMachine {
    /// A machine for a proxy that starts at `now` and does not know whether
    /// the engine runs. The idle clock starts at once, so an engine nobody
    /// uses after the proxy comes up is stopped too.
    pub fn new(policy: IdlePolicy, now: Instant) -> Self {
        let mut machine = Self {
            policy,
            phase: EnginePhase::Unknown,
            connections: BTreeMap::new(),
            watchers: Vec::new(),
            idle_since: now,
            next_check: now,
            last_check: None,
            checking: false,
            check_stale: false,
        };
        machine.restart_idle_clock(now);
        machine
    }

    pub fn policy(&self) -> IdlePolicy {
        self.policy
    }

    pub fn phase(&self) -> EnginePhase {
        self.phase
    }

    /// Connections counted in: relaying, or waiting for a start.
    pub fn connections(&self) -> usize {
        self.connections.len()
    }

    /// The counted connections, in the order of their IDs.
    pub fn connection_views(&self) -> impl Iterator<Item = ConnectionView> + '_ {
        self.connections
            .iter()
            .map(|(id, connection)| ConnectionView {
                id: *id,
                client: connection.client,
                waiting: connection.waiting,
                last_traffic: connection.last_traffic,
            })
    }

    /// Clients that cannot wake the stopped engine by only subscribing.
    pub fn watchers(&self) -> &[ClientId] {
        &self.watchers
    }

    /// Since when the engine has been unused, while that counts towards a
    /// stop.
    pub fn idle_since(&self) -> Option<Instant> {
        self.idle_clock_runs().then_some(self.idle_since)
    }

    /// When the driver must call `on_deadline`, or `None` to wait for the
    /// next event without a timer.
    pub fn deadline(&self) -> Option<Instant> {
        (self.idle_clock_runs() && !self.checking).then_some(self.next_check)
    }

    /// The deadline passed. Returns `Check` when one is due. The driver
    /// reports traffic first: data that moved meanwhile postpones the check.
    pub fn on_deadline(&mut self, now: Instant) -> Option<IdleAction> {
        if !self.deadline().is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.checking = true;
        self.check_stale = false;
        self.last_check = Some(now);
        Some(IdleAction::Check)
    }

    /// A client connected; `id` is the driver's and unique among open
    /// connections. Unless the answer is `Hold`, the connection counts until
    /// the matching `disconnect`, whatever the `Admission`. A held
    /// connection asks again with the same ID.
    pub fn connect(
        &mut self,
        now: Instant,
        id: u64,
        client: Option<ClientId>,
        request: FirstRequest,
    ) -> (Admission, Option<IdleAction>) {
        let asleep = matches!(self.phase, EnginePhase::Stopped | EnginePhase::Stopping);
        let watcher = client.is_some_and(|client| self.watchers.contains(&client));
        if asleep && watcher && request != FirstRequest::Active {
            return (Admission::Hold, None);
        }
        if self.checking {
            self.check_stale = true;
        }
        let (admission, action) = match self.phase {
            EnginePhase::Running => (Admission::Proceed, None),
            EnginePhase::Starting | EnginePhase::Stopping => (Admission::Wait, None),
            EnginePhase::Unknown | EnginePhase::Stopped => {
                self.begin_start();
                (Admission::Wait, Some(IdleAction::Start))
            }
        };
        self.connections.insert(
            id,
            Connection {
                client,
                waiting: admission == Admission::Wait,
                last_traffic: now,
            },
        );
        if self.idle_clock_runs() {
            self.restart_idle_clock(now);
        }
        (admission, action)
    }

    /// Connection `id` carried data, last at `at`. The driver reports this
    /// lazily, before each deadline and each check result.
    pub fn traffic(&mut self, id: u64, at: Instant) {
        let Some(connection) = self.connections.get_mut(&id) else {
            return;
        };
        if at <= connection.last_traffic {
            return;
        }
        connection.last_traffic = at;
        if self.checking && self.last_check.is_some_and(|began| at >= began) {
            self.check_stale = true;
        }
        if self.idle_clock_runs() && at > self.idle_since {
            self.restart_idle_clock(at);
        }
    }

    /// A connection ended, on any path. An ID the machine does not count
    /// (a held connection) changes nothing.
    pub fn disconnect(&mut self, now: Instant, id: u64) {
        if self.connections.remove(&id).is_none() {
            return;
        }
        if self.checking {
            self.check_stale = true;
        }
        if self.idle_clock_runs() {
            self.restart_idle_clock(now);
        }
    }

    /// The start ended. Returns whether the waiting connections may proceed;
    /// if not, the driver closes them and the next connection starts again.
    /// Either way the driver lets held connections ask again only after a
    /// start that worked.
    pub fn start_finished(&mut self, now: Instant, started: bool) -> bool {
        self.phase = if started {
            EnginePhase::Running
        } else {
            EnginePhase::Unknown
        };
        for connection in self.connections.values_mut() {
            connection.waiting = false;
        }
        if self.idle_clock_runs() {
            self.restart_idle_clock(now);
        }
        started
    }

    /// The stop ended. Returns `Start` when connections arrived meanwhile.
    pub fn stop_finished(&mut self, now: Instant, stopped: bool) -> Option<IdleAction> {
        self.phase = if stopped {
            EnginePhase::Stopped
        } else {
            // The connections were not closed, so nobody was cut off.
            self.watchers.clear();
            EnginePhase::Unknown
        };
        if self
            .connections
            .values()
            .any(|connection| connection.waiting)
        {
            self.begin_start();
            return Some(IdleAction::Start);
        }
        if self.idle_clock_runs() {
            self.restart_idle_clock(now);
        }
        None
    }

    /// The check ended. Returns `Stop` when the engine has been idle long
    /// enough.
    pub fn check_finished(&mut self, now: Instant, activity: Activity) -> Option<IdleAction> {
        self.checking = false;
        let stale = std::mem::take(&mut self.check_stale);
        // A connection came, went or carried data, or a start began, while
        // the check ran: what it saw is out of date. The clock was restarted
        // by whatever made it stale.
        if stale || !self.idle_clock_runs() {
            return None;
        }
        match activity {
            Activity::DistroStopped => {
                self.phase = EnginePhase::Stopped;
                None
            }
            Activity::Containers(0) => {
                self.phase = EnginePhase::Running;
                if now >= self.idle_since + self.policy.idle_after {
                    self.phase = EnginePhase::Stopping;
                    // Every open connection has been quiet for the idle
                    // time, or the clock would not have run out.
                    self.watchers = self
                        .connections
                        .values()
                        .filter_map(|connection| connection.client)
                        .collect();
                    self.watchers.sort_unstable();
                    self.watchers.dedup();
                    Some(IdleAction::Stop)
                } else {
                    self.schedule_check(now);
                    None
                }
            }
            Activity::Containers(_) => {
                self.phase = EnginePhase::Running;
                self.restart_idle_clock(now);
                None
            }
            Activity::Unknown => {
                self.restart_idle_clock(now);
                None
            }
        }
    }

    fn begin_start(&mut self) {
        self.phase = EnginePhase::Starting;
        self.watchers.clear();
    }

    /// The engine may be up and nothing uses it, so time counts towards a
    /// stop. Open connections stop the clock only when watchers keep the
    /// engine awake; otherwise their traffic restarts it.
    fn idle_clock_runs(&self) -> bool {
        let phase = matches!(self.phase, EnginePhase::Running | EnginePhase::Unknown);
        let waiting = self
            .connections
            .values()
            .any(|connection| connection.waiting);
        let counted = match self.policy.watchers {
            Watchers::Ignored => false,
            Watchers::KeepAwake => !self.connections.is_empty(),
        };
        phase && !waiting && !counted
    }

    fn restart_idle_clock(&mut self, now: Instant) {
        self.idle_since = now;
        self.schedule_check(now);
    }

    /// The next check comes after `check_every`, or when the idle time is up
    /// if that is sooner, but never sooner than `check_every` after the last.
    fn schedule_check(&mut self, now: Instant) {
        let due = (now + self.policy.check_every).min(self.idle_since + self.policy.idle_after);
        self.next_check = match self.last_check {
            Some(last) => due.max(last + self.policy.check_every),
            None => due,
        };
    }
}

/// Name of the file, in Docklet's data directory, that an idle-stop proxy
/// writes its report to when asked (see `docklet doctor`).
pub const REPORT_FILE: &str = "proxy-connections.txt";

/// Where the proxy's report is, given the data directory.
pub fn report_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REPORT_FILE)
}

/// What a connection the proxy knows of is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Relayed to the engine.
    Open,
    /// Waits for the engine to start.
    Waiting,
    /// A watcher's subscription that waits for the engine without waking it.
    Held,
}

impl ConnectionState {
    const ALL: [Self; 3] = [Self::Open, Self::Waiting, Self::Held];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Waiting => "waiting",
            Self::Held => "held",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == word)
    }
}

/// One connection in the proxy's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedConnection {
    pub state: ConnectionState,
    pub pid: Option<u32>,
    /// File name of the client's executable, when it could be read.
    pub program: Option<String>,
    /// How long it has carried nothing; for a held one, how long it waits.
    pub quiet: Duration,
    /// How long it has been open.
    pub age: Duration,
    pub to_engine: u64,
    pub to_client: u64,
}

/// What an idle-stop proxy says about the engine and its connections, for
/// `docklet doctor`. Written as `key = value` lines, like the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyReport {
    pub phase: EnginePhase,
    pub watchers: Watchers,
    pub idle_after: Duration,
    /// How long the engine has been unused, while that counts towards a stop.
    pub unused_for: Option<Duration>,
    pub connections: Vec<ReportedConnection>,
}

const REPORT_HEADER: &str = "# Written by the Docklet pipe proxy when `docklet doctor` asks.";
/// Same key as the setting, so the report reads like the settings file.
const REPORT_WATCHERS: &str = "watchers-keep-awake";

impl ProxyReport {
    pub fn render(&self) -> String {
        let mut text = format!(
            "{REPORT_HEADER}\nphase = {}\n{REPORT_WATCHERS} = {}\nidle-after = {}\nunused-for = {}\n",
            self.phase.as_str(),
            self.watchers,
            self.idle_after.as_secs(),
            self.unused_for
                .map_or_else(|| "-".to_string(), |unused| unused.as_secs().to_string()),
        );
        for connection in &self.connections {
            text.push_str(&format!(
                "connection = {} {} {} {} {} {} {}\n",
                connection.state.as_str(),
                connection
                    .pid
                    .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                connection.quiet.as_secs(),
                connection.age.as_secs(),
                connection.to_engine,
                connection.to_client,
                // Last, because it may contain spaces.
                connection.program.as_deref().unwrap_or("-"),
            ));
        }
        text
    }

    /// Reads a report back. `None` when a line the report needs is missing
    /// or wrong; lines it does not know are skipped.
    pub fn parse(text: &str) -> Option<Self> {
        let mut phase = None;
        let mut watchers = None;
        let mut idle_after = None;
        let mut unused_for = None;
        let mut connections = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            match key.trim() {
                "phase" => phase = Some(EnginePhase::parse(value)?),
                REPORT_WATCHERS => watchers = Some(Watchers::parse(value)?),
                "idle-after" => idle_after = Some(Duration::from_secs(value.parse().ok()?)),
                "unused-for" => {
                    unused_for = match value {
                        "-" => None,
                        seconds => Some(Duration::from_secs(seconds.parse().ok()?)),
                    }
                }
                "connection" => connections.push(parse_connection(value)?),
                _ => {}
            }
        }
        Some(Self {
            phase: phase?,
            watchers: watchers?,
            idle_after: idle_after?,
            unused_for,
            connections,
        })
    }

    /// What `docklet doctor` says about idle stop: what keeps the engine
    /// awake, and which client that is. A quiet connection that keeps it
    /// awake only because `watchers-keep-awake` is `yes` is a warning.
    pub fn verdict(&self) -> (Level, String, Option<&'static str>) {
        match self.phase {
            EnginePhase::Stopped | EnginePhase::Stopping => {
                let held: Vec<&ReportedConnection> = self
                    .connections
                    .iter()
                    .filter(|connection| connection.state == ConnectionState::Held)
                    .collect();
                let detail = if held.is_empty() {
                    "asleep; the next Docker client wakes it".to_string()
                } else {
                    format!(
                        "asleep; {} of a watcher {} for the engine without waking it: {}",
                        count(held.len(), "connection"),
                        if held.len() == 1 { "waits" } else { "wait" },
                        clients(&held)
                    )
                };
                (Level::Ok, detail, None)
            }
            EnginePhase::Starting => (Level::Ok, "waking up".to_string(), None),
            EnginePhase::Running | EnginePhase::Unknown => self.awake_verdict(),
        }
    }

    fn awake_verdict(&self) -> (Level, String, Option<&'static str>) {
        let (quiet, busy): (Vec<&ReportedConnection>, Vec<&ReportedConnection>) = self
            .connections
            .iter()
            .filter(|connection| connection.state != ConnectionState::Held)
            .partition(|connection| connection.quiet >= self.idle_after);
        let idle = human(self.idle_after);
        if self.watchers == Watchers::KeepAwake && busy.is_empty() && !quiet.is_empty() {
            return (
                Level::Warn,
                format!(
                    "kept awake by {} that carried nothing for {idle} or more: {}; \
                     watchers-keep-awake = yes counts {} as use",
                    count(quiet.len(), "connection"),
                    clients(&quiet),
                    if quiet.len() == 1 { "it" } else { "them" }
                ),
                Some("docklet config set watchers-keep-awake no"),
            );
        }
        let mut detail = if busy.is_empty() {
            match self.unused_for {
                Some(unused) => format!(
                    "unused for {}; sleeps after {idle} without traffic or a running container",
                    human(unused)
                ),
                None => format!("sleeps after {idle} without traffic or a running container"),
            }
        } else {
            let users: Vec<String> = busy
                .iter()
                .map(|connection| {
                    format!(
                        "{}, last data {} ago, {} in {}",
                        client(connection),
                        human(connection.quiet),
                        bytes(connection.to_engine + connection.to_client),
                        human(connection.age)
                    )
                })
                .collect();
            format!("in use by {}", users.join("; "))
        };
        if self.watchers == Watchers::Ignored && !quiet.is_empty() {
            detail.push_str(&format!("; quiet, not counted as use: {}", clients(&quiet)));
        }
        (Level::Ok, detail, None)
    }
}

fn parse_connection(value: &str) -> Option<ReportedConnection> {
    let mut words = value.splitn(7, ' ');
    let state = ConnectionState::parse(words.next()?)?;
    let pid = match words.next()? {
        "-" => None,
        pid => Some(pid.parse().ok()?),
    };
    let mut number = || words.next()?.parse::<u64>().ok();
    let quiet = Duration::from_secs(number()?);
    let age = Duration::from_secs(number()?);
    let to_engine = number()?;
    let to_client = number()?;
    let program = match words.next()?.trim() {
        "-" | "" => None,
        program => Some(program.to_string()),
    };
    Some(ReportedConnection {
        state,
        pid,
        program,
        quiet,
        age,
        to_engine,
        to_client,
    })
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn client(connection: &ReportedConnection) -> String {
    match (&connection.program, connection.pid) {
        (Some(program), Some(pid)) => format!("{program} (pid {pid})"),
        (None, Some(pid)) => format!("pid {pid}"),
        (Some(program), None) => program.clone(),
        (None, None) => "an unknown client".to_string(),
    }
}

/// The clients of `connections`, each named once.
fn clients(connections: &[&ReportedConnection]) -> String {
    let mut names: Vec<String> = connections.iter().map(|c| client(c)).collect();
    names.dedup();
    names.join(", ")
}

fn human(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..120 => format!("{seconds} s"),
        120..7200 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}

fn bytes(count: u64) -> String {
    match count {
        0..10_000 => format!("{count} bytes"),
        10_000..10_000_000 => format!("{} KB", count / 1000),
        _ => format!("{} MB", count / 1_000_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SECOND: Duration = Duration::from_secs(1);
    const ALICE: ClientId = ClientId {
        pid: 100,
        started: 1,
    };
    const BOB: ClientId = ClientId {
        pid: 200,
        started: 2,
    };

    /// Five minutes idle, a check every minute.
    fn policy() -> IdlePolicy {
        IdlePolicy::new(Duration::from_secs(300)).with_check_every(Duration::from_secs(60))
    }

    /// A fake clock and a machine whose engine is known to run.
    struct Sim {
        start: Instant,
        machine: IdleMachine,
    }

    impl Sim {
        fn new() -> Self {
            Self::with(policy())
        }

        fn with(policy: IdlePolicy) -> Self {
            let start = Instant::now();
            let mut sim = Self {
                start,
                machine: IdleMachine::new(policy, start),
            };
            // One client brings the engine up and leaves at t = 0.
            assert_eq!(
                sim.connect(0, 0),
                (Admission::Wait, Some(IdleAction::Start))
            );
            assert!(sim.machine.start_finished(sim.at(0), true));
            sim.disconnect(0, 0);
            sim
        }

        fn at(&self, secs: u64) -> Instant {
            self.start + Duration::from_secs(secs)
        }

        fn deadline_secs(&self) -> Option<u64> {
            self.machine
                .deadline()
                .map(|deadline| (deadline - self.start).as_secs())
        }

        /// An anonymous client with request `Unread`, as every connection
        /// first asks.
        fn connect(&mut self, id: u64, secs: u64) -> (Admission, Option<IdleAction>) {
            self.connect_as(id, secs, None, FirstRequest::Unread)
        }

        fn connect_as(
            &mut self,
            id: u64,
            secs: u64,
            client: Option<ClientId>,
            request: FirstRequest,
        ) -> (Admission, Option<IdleAction>) {
            let now = self.at(secs);
            self.machine.connect(now, id, client, request)
        }

        fn disconnect(&mut self, id: u64, secs: u64) {
            let now = self.at(secs);
            self.machine.disconnect(now, id);
        }

        fn traffic(&mut self, id: u64, secs: u64) {
            let at = self.at(secs);
            self.machine.traffic(id, at);
        }

        /// Fires the deadline at `secs` and answers the check with
        /// `activity`.
        fn check(&mut self, secs: u64, activity: Activity) -> Option<IdleAction> {
            assert_eq!(self.deadline_secs(), Some(secs), "check due at {secs} s");
            let now = self.at(secs);
            assert_eq!(self.machine.on_deadline(now), Some(IdleAction::Check));
            assert_eq!(self.machine.deadline(), None, "no timer while checking");
            self.machine.check_finished(now, activity)
        }

        /// Runs the checks a quiet engine gets until one asks for a stop,
        /// and returns when that was. Gives up after `limit` seconds.
        fn run_until_stop(&mut self, limit: u64) -> Option<u64> {
            while let Some(due) = self.deadline_secs() {
                if due > limit {
                    return None;
                }
                if self.check(due, Activity::Containers(0)) == Some(IdleAction::Stop) {
                    return Some(due);
                }
            }
            None
        }
    }

    #[test]
    fn policy_clamps_the_check_interval() {
        let short = IdlePolicy::new(Duration::ZERO);
        assert_eq!(short.check_every(), IdlePolicy::MIN_CHECK_EVERY);
        let long = IdlePolicy::new(Duration::from_secs(3600));
        assert_eq!(long.check_every(), IdlePolicy::DEFAULT_CHECK_EVERY);
        let custom = long.with_check_every(Duration::ZERO);
        assert_eq!(custom.check_every(), IdlePolicy::MIN_CHECK_EVERY);
        // Huge values do not overflow the clock arithmetic.
        let huge = IdlePolicy::new(Duration::MAX).with_check_every(Duration::MAX);
        let mut machine = IdleMachine::new(huge, Instant::now());
        assert!(machine.deadline().is_some());
        machine.disconnect(Instant::now(), 7);
    }

    #[test]
    fn watchers_are_ignored_unless_the_policy_says_otherwise() {
        assert_eq!(policy().watchers(), Watchers::Ignored);
        assert_eq!(Watchers::default(), Watchers::Ignored);
        let keep = policy().with_watchers(Watchers::KeepAwake);
        assert_eq!(keep.watchers(), Watchers::KeepAwake);
        assert_eq!(Watchers::parse(" YES "), Some(Watchers::KeepAwake));
        assert_eq!(Watchers::parse("no"), Some(Watchers::Ignored));
        for wrong in ["", "on", "off", "true", "1", "y"] {
            assert_eq!(Watchers::parse(wrong), None, "{wrong:?}");
        }
        for value in [Watchers::Ignored, Watchers::KeepAwake] {
            assert_eq!(Watchers::parse(&value.to_string()), Some(value));
        }
    }

    #[test]
    fn an_unused_engine_stops_after_the_idle_time() {
        let mut sim = Sim::new();
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(300, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
        assert_eq!(sim.machine.phase(), EnginePhase::Stopping);
        assert_eq!(sim.machine.stop_finished(sim.at(301), true), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Stopped);
        // A stopped engine schedules nothing: no process runs until a client
        // connects.
        assert_eq!(sim.machine.deadline(), None);
        assert!(sim.machine.watchers().is_empty(), "nobody was cut off");
    }

    #[test]
    fn with_watchers_kept_awake_open_connections_pause_the_clock() {
        let mut sim = Sim::with(policy().with_watchers(Watchers::KeepAwake));
        assert_eq!(sim.check(60, Activity::Containers(0)), None);
        assert_eq!(sim.connect(1, 100), (Admission::Proceed, None));
        assert_eq!(sim.connect(2, 110), (Admission::Proceed, None));
        // A long-lived connection (`docker events`) keeps the engine in use
        // and no check runs meanwhile, however quiet it is.
        assert_eq!(sim.machine.deadline(), None);
        assert_eq!(sim.machine.on_deadline(sim.at(10_000)), None);
        sim.disconnect(1, 1000);
        assert_eq!(sim.machine.deadline(), None, "one connection is still open");
        sim.disconnect(2, 2000);
        assert_eq!(sim.machine.connections(), 0);
        // The idle time counts from the last disconnect.
        for secs in [2060, 2120, 2180, 2240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(2300, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
    }

    #[test]
    fn a_quiet_open_connection_does_not_keep_the_engine_awake() {
        let mut sim = Sim::new();
        // `docker events`: one request at 10 s, then nothing.
        assert_eq!(
            sim.connect_as(1, 10, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        sim.traffic(1, 10);
        assert_eq!(sim.run_until_stop(10_000), Some(310));
        // Its client is remembered as a watcher, to be closed with the stop.
        assert_eq!(sim.machine.watchers(), [ALICE]);
        assert_eq!(sim.machine.stop_finished(sim.at(312), true), None);
        sim.disconnect(1, 312);
        assert_eq!(sim.machine.connections(), 0);
        assert_eq!(sim.machine.deadline(), None);
    }

    #[test]
    fn a_watcher_that_reconnects_every_26_seconds_never_wakes_the_engine() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert_eq!(sim.machine.stop_finished(sim.at(303), true), None);
        sim.disconnect(1, 303);
        // A day of reconnects, each with the requests a watcher opens with,
        // and some that give up and come back.
        let mut id = 10;
        for secs in (329..86_400).step_by(26) {
            id += 1;
            for request in [FirstRequest::Unread, FirstRequest::Passive] {
                assert_eq!(
                    sim.connect_as(id, secs, Some(ALICE), request),
                    (Admission::Hold, None),
                    "{request:?} at {secs} s"
                );
            }
            sim.disconnect(id, secs + 20);
        }
        assert_eq!(sim.machine.phase(), EnginePhase::Stopped);
        assert_eq!(sim.machine.connections(), 0);
        assert_eq!(sim.machine.deadline(), None, "nothing runs while asleep");
        // Another client wakes it; the held watcher then asks again and is
        // let in, and is no longer held back.
        assert_eq!(
            sim.connect_as(500, 90_000, Some(BOB), FirstRequest::Unread),
            (Admission::Wait, Some(IdleAction::Start))
        );
        assert!(sim.machine.watchers().is_empty());
        assert!(sim.machine.start_finished(sim.at(90_005), true));
        assert_eq!(
            sim.connect_as(id, 90_005, Some(ALICE), FirstRequest::Passive),
            (Admission::Proceed, None)
        );
        // Bob leaves; Alice is quiet again, so the engine sleeps once more
        // and she is a watcher again.
        sim.disconnect(500, 90_010);
        assert_eq!(sim.run_until_stop(100_000), Some(90_310));
        assert_eq!(sim.machine.watchers(), [ALICE]);
    }

    #[test]
    fn a_watcher_that_asks_for_real_work_wakes_the_engine() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert_eq!(sim.machine.stop_finished(sim.at(302), true), None);
        sim.disconnect(1, 302);
        // An IDE that watched, then the user clicks "run".
        assert_eq!(
            sim.connect_as(2, 400, Some(ALICE), FirstRequest::Active),
            (Admission::Wait, Some(IdleAction::Start))
        );
        // A process the stop did not cut off wakes it whatever it asks.
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert_eq!(sim.machine.stop_finished(sim.at(302), true), None);
        sim.disconnect(1, 302);
        assert_eq!(
            sim.connect_as(2, 400, Some(BOB), FirstRequest::Passive),
            (Admission::Wait, Some(IdleAction::Start))
        );
        // A client the proxy could not identify is never held.
        let mut sim = Sim::new();
        assert_eq!(sim.connect(1, 0), (Admission::Proceed, None));
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert!(sim.machine.watchers().is_empty());
    }

    #[test]
    fn a_watcher_reconnecting_during_the_stop_is_held_and_others_wait() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert_eq!(
            sim.connect_as(2, 301, Some(ALICE), FirstRequest::Unread),
            (Admission::Hold, None)
        );
        assert_eq!(sim.machine.stop_finished(sim.at(303), true), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Stopped);
        // Bob during a stop waits and starts the engine right after it.
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        assert_eq!(sim.run_until_stop(10_000), Some(300));
        assert_eq!(
            sim.connect_as(2, 301, Some(BOB), FirstRequest::Unread),
            (Admission::Wait, None)
        );
        assert_eq!(
            sim.machine.stop_finished(sim.at(303), true),
            Some(IdleAction::Start)
        );
        assert!(sim.machine.watchers().is_empty());
    }

    #[test]
    fn a_client_that_works_and_then_goes_quiet_stops_counting() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        // It works for 20 minutes, a burst every 10 s: each one moves the
        // next check out, so none runs.
        for secs in (10..=1200).step_by(10) {
            sim.traffic(1, secs);
            assert_eq!(sim.deadline_secs(), Some(secs + 60), "at {secs} s");
        }
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
        // Then it goes quiet with the connection open: five minutes later
        // the engine stops.
        assert_eq!(sim.run_until_stop(10_000), Some(1500));
        assert_eq!(sim.machine.watchers(), [ALICE]);
    }

    #[test]
    fn a_busy_client_keeps_the_engine_awake() {
        let mut sim = Sim::new();
        assert_eq!(sim.connect(1, 0), (Admission::Proceed, None));
        // A pull or a build progress stream: data every 30 s for two days.
        // The driver reports traffic before each deadline, and the deadline
        // that fires finds it moved: no check, no stop.
        for now in (30..2 * 86_400).step_by(30) {
            sim.traffic(1, now);
            assert_eq!(sim.machine.on_deadline(sim.at(now)), None, "at {now} s");
        }
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
        assert_eq!(sim.machine.connections(), 1);
    }

    #[test]
    fn data_during_a_check_makes_its_result_stale() {
        let mut sim = Sim::new();
        assert_eq!(sim.connect(1, 0), (Admission::Proceed, None));
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(sim.deadline_secs(), Some(300));
        assert_eq!(
            sim.machine.on_deadline(sim.at(300)),
            Some(IdleAction::Check)
        );
        sim.traffic(1, 301);
        assert_eq!(
            sim.machine
                .check_finished(sim.at(302), Activity::Containers(0)),
            None
        );
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
        // The clock restarted with the data.
        assert_eq!(sim.deadline_secs(), Some(361));
        // Traffic reported late, but from before the check, changes nothing
        // about the check; older traffic changes nothing at all.
        sim.traffic(1, 100);
        assert_eq!(sim.deadline_secs(), Some(361));
    }

    #[test]
    fn traffic_of_an_unknown_connection_is_ignored() {
        let mut sim = Sim::new();
        sim.traffic(42, 30);
        assert_eq!(sim.deadline_secs(), Some(60));
        sim.disconnect(42, 30);
        assert_eq!(sim.deadline_secs(), Some(60));
    }

    #[test]
    fn a_running_container_restarts_the_clock() {
        let mut sim = Sim::new();
        assert_eq!(sim.check(60, Activity::Containers(0)), None);
        assert_eq!(sim.check(120, Activity::Containers(2)), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
        // The container keeps the engine up for as long as it runs.
        for secs in [180, 240, 300, 360] {
            assert_eq!(sim.check(secs, Activity::Containers(1)), None);
        }
        // It ended; five more minutes without one stop the engine.
        for secs in [420, 480, 540, 600] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(660, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
    }

    #[test]
    fn a_running_container_keeps_a_watched_engine_up() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        for secs in [60, 120, 180, 240, 300, 360, 420] {
            assert_eq!(sim.check(secs, Activity::Containers(1)), None);
        }
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
    }

    #[test]
    fn a_failed_check_never_stops_the_engine() {
        let mut sim = Sim::new();
        for secs in [60, 120, 180, 240, 300, 360] {
            assert_eq!(sim.check(secs, Activity::Unknown), None);
        }
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
    }

    #[test]
    fn a_distro_stopped_from_outside_cancels_the_checks() {
        let mut sim = Sim::new();
        assert_eq!(sim.check(60, Activity::DistroStopped), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Stopped);
        assert_eq!(sim.machine.deadline(), None);
        assert_eq!(
            sim.connect(1, 500),
            (Admission::Wait, Some(IdleAction::Start))
        );
    }

    #[test]
    fn concurrent_connections_share_one_start() {
        let start = Instant::now();
        let mut machine = IdleMachine::new(policy(), start);
        let unread = FirstRequest::Unread;
        assert_eq!(machine.phase(), EnginePhase::Unknown);
        assert_eq!(
            machine.connect(start, 0, None, unread),
            (Admission::Wait, Some(IdleAction::Start))
        );
        for id in 1..3 {
            assert_eq!(
                machine.connect(start + SECOND, id, None, unread),
                (Admission::Wait, None)
            );
        }
        assert_eq!(machine.deadline(), None, "no check while starting");
        assert!(machine.start_finished(start + 20 * SECOND, true));
        assert_eq!(machine.phase(), EnginePhase::Running);
        assert_eq!(
            machine.connect(start + 21 * SECOND, 3, None, unread),
            (Admission::Proceed, None)
        );
        assert_eq!(machine.connections(), 4);
        assert!(machine.connection_views().all(|view| !view.waiting));
    }

    #[test]
    fn a_failed_start_releases_the_waiters_and_the_next_connection_retries() {
        let start = Instant::now();
        let mut machine = IdleMachine::new(policy(), start);
        let unread = FirstRequest::Unread;
        assert_eq!(
            machine.connect(start, 0, None, unread).1,
            Some(IdleAction::Start)
        );
        assert_eq!(machine.connect(start, 1, None, unread).1, None);
        assert!(!machine.start_finished(start + SECOND, false));
        assert_eq!(machine.phase(), EnginePhase::Unknown);
        // The driver closes both waiting connections.
        machine.disconnect(start + SECOND, 0);
        machine.disconnect(start + SECOND, 1);
        // The distro may be half up; checks find out and stop it if unused.
        assert!(machine.deadline().is_some());
        assert_eq!(
            machine.connect(start + 2 * SECOND, 2, None, unread),
            (Admission::Wait, Some(IdleAction::Start))
        );
    }

    #[test]
    fn a_connection_during_a_stop_starts_the_engine_again() {
        let mut sim = Sim::new();
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(300, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
        assert_eq!(sim.connect(1, 301), (Admission::Wait, None));
        assert_eq!(sim.connect(2, 302), (Admission::Wait, None));
        assert_eq!(
            sim.machine.stop_finished(sim.at(305), true),
            Some(IdleAction::Start)
        );
        assert_eq!(sim.machine.phase(), EnginePhase::Starting);
        assert!(sim.machine.start_finished(sim.at(320), true));
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
    }

    #[test]
    fn a_failed_stop_is_retried_only_after_another_idle_period() {
        let mut sim = Sim::new();
        assert_eq!(
            sim.connect_as(1, 0, Some(ALICE), FirstRequest::Unread),
            (Admission::Proceed, None)
        );
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(300, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
        assert_eq!(sim.machine.stop_finished(sim.at(310), false), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Unknown);
        // Nothing was closed, so nobody is held back.
        assert!(sim.machine.watchers().is_empty());
        for secs in [370, 430, 490, 550] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(610, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
    }

    #[test]
    fn a_check_overtaken_by_a_connection_is_ignored() {
        let mut sim = Sim::new();
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(sim.deadline_secs(), Some(300));
        assert_eq!(
            sim.machine.on_deadline(sim.at(300)),
            Some(IdleAction::Check)
        );
        // A client comes and goes while the last check runs.
        assert_eq!(sim.connect(1, 301), (Admission::Proceed, None));
        sim.disconnect(1, 302);
        assert_eq!(
            sim.machine
                .check_finished(sim.at(303), Activity::Containers(0)),
            None
        );
        assert_eq!(sim.machine.phase(), EnginePhase::Running);
        // The clock restarted at the disconnect.
        assert_eq!(sim.deadline_secs(), Some(362));
    }

    #[test]
    fn checks_never_run_more_often_than_the_interval() {
        let mut sim = Sim::new();
        assert_eq!(sim.check(60, Activity::Containers(0)), None);
        // Short connections right after a check would each restart the
        // clock; the next check still waits for the interval.
        assert_eq!(sim.connect(1, 61), (Admission::Proceed, None));
        sim.disconnect(1, 61);
        assert_eq!(sim.deadline_secs(), Some(121));
        let policy =
            IdlePolicy::new(Duration::from_secs(10)).with_check_every(Duration::from_secs(60));
        let start = Instant::now();
        let mut machine = IdleMachine::new(policy, start);
        assert_eq!(machine.deadline(), Some(start + 10 * SECOND));
        assert_eq!(
            machine.on_deadline(start + 10 * SECOND),
            Some(IdleAction::Check)
        );
        assert_eq!(
            machine.check_finished(start + 10 * SECOND, Activity::Containers(1)),
            None
        );
        // Idle time is shorter than the interval: the interval wins.
        assert_eq!(machine.deadline(), Some(start + 70 * SECOND));
    }

    #[test]
    fn a_deadline_fired_early_does_nothing() {
        let sim = Sim::new();
        let mut machine = sim.machine;
        assert_eq!(machine.on_deadline(sim.start + 59 * SECOND), None);
        assert!(machine.deadline().is_some());
    }

    #[test]
    fn request_lines_are_classified() {
        let passive = [
            "HEAD /_ping HTTP/1.1\r\nHost: docker\r\n\r\n",
            "GET /_ping HTTP/1.1\r\n",
            "GET /v1.47/version HTTP/1.1\r\n",
            "GET /v1.51/info HTTP/1.1\r\n",
            "GET /v1.47/events?filters=%7B%7D HTTP/1.1\r\n",
            "GET /events HTTP/1.1\n",
            "POST /grpc HTTP/1.1\r\nUpgrade: h2c\r\n",
            "POST /v1.47/grpc HTTP/1.1\r\n",
        ];
        for line in passive {
            assert_eq!(
                classify_request(line.as_bytes()),
                FirstRequest::Passive,
                "{line:?}"
            );
        }
        let active = [
            "GET /v1.47/containers/json HTTP/1.1\r\n",
            "POST /v1.47/containers/create HTTP/1.1\r\n",
            "POST /v1.47/build HTTP/1.1\r\n",
            "POST /session HTTP/1.1\r\n",
            "POST /events HTTP/1.1\r\n",
            "GET /v1.47/events/x HTTP/1.1\r\n",
            "GET /vx/events HTTP/1.1\r\n",
            "GET /v/events HTTP/1.1\r\n",
            "GET /_ping\r\n",
            "GET /_ping HTTP/1.1 extra\r\n",
            "GET /_ping FTP/1.0\r\n",
            "\r\n",
            "\u{16}\u{3}\u{1} binary\n",
        ];
        for line in active {
            assert_eq!(
                classify_request(line.as_bytes()),
                FirstRequest::Active,
                "{line:?}"
            );
        }
        assert_eq!(classify_request(b""), FirstRequest::Unread);
        assert_eq!(classify_request(b"GET /events HT"), FirstRequest::Unread);
        assert_eq!(classify_request(&[0xff, b'\n']), FirstRequest::Active);
        let endless = vec![b'a'; MAX_REQUEST_LINE];
        assert_eq!(classify_request(&endless), FirstRequest::Active);
    }

    fn connection(state: ConnectionState, pid: u32, quiet: u64) -> ReportedConnection {
        ReportedConnection {
            state,
            pid: Some(pid),
            program: Some("com.docker.build.exe".to_string()),
            quiet: Duration::from_secs(quiet),
            age: Duration::from_secs(7200),
            to_engine: 410,
            to_client: 2300,
        }
    }

    fn report(phase: EnginePhase, connections: Vec<ReportedConnection>) -> ProxyReport {
        ProxyReport {
            phase,
            watchers: Watchers::Ignored,
            idle_after: Duration::from_secs(600),
            unused_for: Some(Duration::from_secs(42)),
            connections,
        }
    }

    #[test]
    fn a_report_is_read_back_as_written() {
        let mut written = report(
            EnginePhase::Running,
            vec![
                connection(ConnectionState::Open, 4242, 1500),
                ReportedConnection {
                    state: ConnectionState::Held,
                    pid: None,
                    program: Some("My Tool.exe".to_string()),
                    quiet: Duration::ZERO,
                    age: Duration::from_secs(3),
                    to_engine: 0,
                    to_client: 0,
                },
                ReportedConnection {
                    state: ConnectionState::Waiting,
                    pid: Some(7),
                    program: None,
                    quiet: Duration::from_secs(1),
                    age: Duration::from_secs(1),
                    to_engine: 9,
                    to_client: 0,
                },
            ],
        );
        let text = written.render();
        assert!(text.starts_with('#'));
        assert!(
            text.contains("\nconnection = open 4242 1500 7200 410 2300 com.docker.build.exe\n")
        );
        assert!(text.contains("\nconnection = held - 0 3 0 0 My Tool.exe\n"));
        assert_eq!(ProxyReport::parse(&text), Some(written.clone()));
        written.unused_for = None;
        written.watchers = Watchers::KeepAwake;
        written.connections.clear();
        assert_eq!(ProxyReport::parse(&written.render()), Some(written));
    }

    #[test]
    fn a_report_missing_a_line_or_with_a_wrong_one_is_not_read() {
        let good = report(EnginePhase::Stopped, Vec::new()).render();
        assert!(ProxyReport::parse(&good).is_some());
        assert!(ProxyReport::parse(&format!("{good}future-key = 1\n")).is_some());
        for (line, replacement) in [
            ("phase = stopped", "phase = asleep"),
            ("phase = stopped", ""),
            ("watchers-keep-awake = no", "watchers-keep-awake = maybe"),
            ("idle-after = 600", "idle-after = ten"),
            ("unused-for = 42", "unused-for = soon"),
        ] {
            let text = good.replace(line, replacement);
            assert_eq!(ProxyReport::parse(&text), None, "{replacement:?}");
        }
        let connection = format!("{good}connection = open 1 2 3\n");
        assert_eq!(ProxyReport::parse(&connection), None);
        assert_eq!(ProxyReport::parse(""), None);
    }

    #[test]
    fn the_verdict_names_what_keeps_the_engine_awake() {
        let (level, detail, fix) = report(
            EnginePhase::Running,
            vec![
                connection(ConnectionState::Open, 4242, 1500),
                ReportedConnection {
                    program: Some("docker.exe".to_string()),
                    ..connection(ConnectionState::Open, 77, 12)
                },
            ],
        )
        .verdict();
        assert_eq!(level, Level::Ok);
        assert_eq!(fix, None);
        assert_eq!(
            detail,
            "in use by docker.exe (pid 77), last data 12 s ago, 2710 bytes in 2 h; \
             quiet, not counted as use: com.docker.build.exe (pid 4242)"
        );
        let (level, detail, _) = report(EnginePhase::Running, Vec::new()).verdict();
        assert_eq!(level, Level::Ok);
        assert_eq!(
            detail,
            "unused for 42 s; sleeps after 10 min without traffic or a running container"
        );
    }

    #[test]
    fn a_quiet_connection_kept_awake_by_the_setting_is_a_warning() {
        let mut watched = report(
            EnginePhase::Running,
            vec![connection(ConnectionState::Open, 4242, 1500)],
        );
        watched.watchers = Watchers::KeepAwake;
        watched.unused_for = None;
        let (level, detail, fix) = watched.verdict();
        assert_eq!(level, Level::Warn);
        assert_eq!(
            detail,
            "kept awake by 1 connection that carried nothing for 10 min or more: \
             com.docker.build.exe (pid 4242); watchers-keep-awake = yes counts it as use"
        );
        assert_eq!(fix, Some("docklet config set watchers-keep-awake no"));
        // A connection that still carries data is use either way.
        watched.connections[0].quiet = Duration::from_secs(5);
        assert_eq!(watched.verdict().0, Level::Ok);
    }

    #[test]
    fn the_verdict_of_a_sleeping_engine_names_the_held_watchers() {
        let (level, detail, _) = report(EnginePhase::Stopped, Vec::new()).verdict();
        assert_eq!(level, Level::Ok);
        assert_eq!(detail, "asleep; the next Docker client wakes it");
        let held = report(
            EnginePhase::Stopped,
            vec![
                connection(ConnectionState::Held, 4242, 30),
                connection(ConnectionState::Held, 4242, 4),
            ],
        );
        assert_eq!(
            held.verdict().1,
            "asleep; 2 connections of a watcher wait for the engine without waking it: \
             com.docker.build.exe (pid 4242)"
        );
        assert_eq!(
            report(EnginePhase::Starting, Vec::new()).verdict().1,
            "waking up"
        );
    }

    #[test]
    fn durations_and_sizes_read_well() {
        assert_eq!(human(Duration::from_secs(59)), "59 s");
        assert_eq!(human(Duration::from_secs(119)), "119 s");
        assert_eq!(human(Duration::from_secs(600)), "10 min");
        assert_eq!(human(Duration::from_secs(7200)), "2 h");
        assert_eq!(bytes(9_999), "9999 bytes");
        assert_eq!(bytes(12_345), "12 KB");
        assert_eq!(bytes(45_000_000), "45 MB");
        assert_eq!(
            report_path(Path::new("data")),
            Path::new("data").join(REPORT_FILE)
        );
    }

    /// Engine hooks that answer from fields and count the calls.
    struct FakeEngine {
        running: io::Result<bool>,
        containers: io::Result<usize>,
        container_calls: AtomicUsize,
    }

    impl EngineHooks for FakeEngine {
        fn start_engine(&self) -> io::Result<()> {
            Ok(())
        }
        fn stop_distro(&self) -> io::Result<()> {
            Ok(())
        }
        fn distro_running(&self) -> io::Result<bool> {
            match &self.running {
                Ok(running) => Ok(*running),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
        fn running_containers(&self) -> io::Result<usize> {
            self.container_calls.fetch_add(1, Ordering::Relaxed);
            match &self.containers {
                Ok(count) => Ok(*count),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    fn fake(running: io::Result<bool>, containers: io::Result<usize>) -> FakeEngine {
        FakeEngine {
            running,
            containers,
            container_calls: AtomicUsize::new(0),
        }
    }

    #[test]
    fn a_check_never_asks_dockerd_of_a_stopped_distro() {
        let engine = fake(Ok(false), Ok(3));
        assert_eq!(check_activity(&engine), Activity::DistroStopped);
        assert_eq!(engine.container_calls.load(Ordering::Relaxed), 0);
        let engine = fake(Err(io::Error::other("wsl hung")), Ok(3));
        assert_eq!(check_activity(&engine), Activity::Unknown);
        assert_eq!(engine.container_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_check_counts_the_containers_of_a_running_distro() {
        assert_eq!(
            check_activity(&fake(Ok(true), Ok(2))),
            Activity::Containers(2)
        );
        assert_eq!(
            check_activity(&fake(Ok(true), Err(io::Error::other("no dockerd")))),
            Activity::Unknown
        );
    }
}
