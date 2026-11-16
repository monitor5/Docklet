//! Idle stop and wake on connect: drives `docklet_core::idle::IdleMachine`.
//!
//! One controller task owns the machine, so every decision is taken in one
//! place and in the order events arrived. Connections talk to it over a
//! channel; engine calls run on the blocking pool and report back over the
//! same channel. The controller waits on that channel, and on a timer only
//! when the machine asks for one, so an engine that is stopped or in use
//! costs nothing.
//!
//! Relays count the bytes of their connection in a shared [`Traffic`]; the
//! controller reads those counters only when it is about to decide, before
//! each deadline and each check result, so data costs no message.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use docklet_core::idle::{
    Activity, Admission, ClientId, ConnectionState, EngineHooks, FirstRequest, IdleAction,
    IdleMachine, IdlePolicy, ProxyReport, ReportedConnection, check_activity,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, WeakUnboundedSender};
use tokio::sync::{oneshot, watch};

use crate::control::ReportSignal;

/// Turns on idle stop and wake on connect for a `PipeProxy`.
#[derive(Clone)]
pub struct IdleStop {
    policy: IdlePolicy,
    hooks: Arc<dyn EngineHooks>,
    report: Option<PathBuf>,
}

impl IdleStop {
    /// Stops the engine through `hooks` after it was unused for the policy's
    /// idle time, and starts it again when a client connects.
    pub fn new(policy: IdlePolicy, hooks: impl EngineHooks + 'static) -> Self {
        Self {
            policy,
            hooks: Arc::new(hooks),
            report: None,
        }
    }

    /// Writes a report of the open connections to `path` whenever
    /// `control::request_report` asks for one (`docklet doctor` does).
    pub fn with_report(mut self, path: PathBuf) -> Self {
        self.report = Some(path);
        self
    }
}

impl std::fmt::Debug for IdleStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleStop")
            .field("policy", &self.policy)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

/// The process at the other end of a connection, as far as it could be
/// found out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Client {
    pub(crate) id: ClientId,
    /// File name of its executable.
    pub(crate) program: Option<String>,
}

/// What one connection carried, and when it last did. Written by the relay,
/// read by the controller.
#[derive(Debug)]
pub(crate) struct Traffic {
    base: Instant,
    /// Milliseconds after `base`.
    last: AtomicU64,
    to_engine: AtomicU64,
    to_client: AtomicU64,
}

impl Traffic {
    fn new(base: Instant) -> Self {
        let traffic = Self {
            base,
            last: AtomicU64::new(0),
            to_engine: AtomicU64::new(0),
            to_client: AtomicU64::new(0),
        };
        traffic.touch();
        traffic
    }

    pub(crate) fn to_engine(&self, bytes: usize) {
        self.to_engine.fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    pub(crate) fn to_client(&self, bytes: usize) {
        self.to_client.fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    fn touch(&self) {
        let millis = u64::try_from(self.base.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last.fetch_max(millis, Ordering::Relaxed);
    }

    fn last(&self) -> Instant {
        self.base + Duration::from_millis(self.last.load(Ordering::Relaxed))
    }
}

enum Event {
    /// A connection arrived, or a held one asks again.
    Connect {
        id: u64,
        client: Option<Client>,
        request: FirstRequest,
        traffic: Arc<Traffic>,
        reply: oneshot::Sender<Reply>,
    },
    Disconnect(u64),
    Started(bool),
    Stopped(bool),
    Checked(Activity),
    /// Write the report and signal that it is there.
    Report,
}

enum Reply {
    /// Relay; the number is the cut epoch the connection belongs to.
    Proceed(u64),
    Refused,
    /// Do not relay yet; ask again when this resolves or the client sent
    /// more.
    Hold(oneshot::Receiver<()>),
}

/// What a connection is to do.
pub(crate) enum Admit {
    Relay(Admitted),
    /// Keep the client connected without relaying, and call `admit` again
    /// when the receiver resolves (the engine started) or the client sent
    /// more.
    Hold(oneshot::Receiver<()>),
    /// Close the connection: the engine did not start.
    Refused,
}

/// A connection that may be relayed.
pub(crate) struct Admitted {
    /// Counts the connection in until dropped.
    pub(crate) guard: ConnectionGuard,
    pub(crate) cut: Cut,
}

/// Resolves when an idle stop closed the connections that were open before
/// it.
pub(crate) struct Cut {
    epochs: watch::Receiver<u64>,
    epoch: u64,
}

impl Cut {
    pub(crate) async fn wait(mut self) {
        loop {
            if *self.epochs.borrow_and_update() != self.epoch {
                return;
            }
            // Every round waits for a change; with the controller gone there
            // is none to come.
            if self.epochs.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Handle to the controller task.
#[derive(Clone)]
pub(crate) struct Lifecycle {
    events: UnboundedSender<Event>,
    epochs: watch::Receiver<u64>,
    base: Instant,
}

impl Lifecycle {
    /// Starts the controller on the current runtime. It ends when every
    /// handle and every connection guard is gone.
    pub(crate) fn spawn(idle: IdleStop, trace: bool) -> Self {
        let signal = match &idle.report {
            Some(_) => match ReportSignal::create() {
                Ok(signal) => Some(Arc::new(signal)),
                Err(err) => {
                    if trace {
                        eprintln!("[engine] no connection report: {err}");
                    }
                    None
                }
            },
            None => None,
        };
        Self::spawn_with(idle, trace, signal)
    }

    fn spawn_with(idle: IdleStop, trace: bool, signal: Option<Arc<ReportSignal>>) -> Self {
        let (events, inbox) = mpsc::unbounded_channel();
        let (cuts, epochs) = watch::channel(0);
        let base = Instant::now();
        let IdleStop {
            policy,
            hooks,
            report,
        } = idle;
        if let Some(signal) = &signal {
            listen_for_reports(Arc::clone(signal), events.downgrade());
        }
        let controller = Controller {
            machine: IdleMachine::new(policy, base),
            hooks,
            trace,
            outbox: events.downgrade(),
            waiting: Vec::new(),
            held: Vec::new(),
            open: HashMap::new(),
            cuts,
            report,
            signal,
        };
        tokio::spawn(controller.run(inbox));
        Self {
            events,
            epochs,
            base,
        }
    }

    /// A fresh counter for a connection that is about to ask for admission.
    pub(crate) fn traffic(&self) -> Arc<Traffic> {
        Arc::new(Traffic::new(self.base))
    }

    /// Counts a new connection in and waits until the engine can serve it,
    /// or says to hold it or close it. `id` is unique among the proxy's
    /// connections; a held connection asks again with the same one. The
    /// connection counts as in use until the guard in `Admit::Relay`, or on
    /// any other answer this future, is dropped.
    pub(crate) async fn admit(
        &self,
        id: u64,
        client: Option<Client>,
        request: FirstRequest,
        traffic: Arc<Traffic>,
    ) -> Admit {
        let (reply, ready) = oneshot::channel();
        let connect = Event::Connect {
            id,
            client,
            request,
            traffic,
            reply,
        };
        if self.events.send(connect).is_err() {
            return Admit::Refused;
        }
        // Created right after the controller heard of the connection, with no
        // await in between, so every path out of here counts it out again.
        // For a held connection that is a no-op: it was never counted.
        let guard = ConnectionGuard {
            events: self.events.clone(),
            id,
        };
        match ready.await {
            Ok(Reply::Proceed(epoch)) => Admit::Relay(Admitted {
                guard,
                cut: Cut {
                    epochs: self.epochs.clone(),
                    epoch,
                },
            }),
            Ok(Reply::Hold(release)) => Admit::Hold(release),
            Ok(Reply::Refused) | Err(_) => Admit::Refused,
        }
    }

    /// Writes the report as if `docklet doctor` had asked.
    #[cfg(test)]
    pub(crate) fn request_report(&self) {
        let _ = self.events.send(Event::Report);
    }
}

/// Counts its connection as open until dropped, on every exit path of the
/// relay including errors and panics.
pub(crate) struct ConnectionGuard {
    events: UnboundedSender<Event>,
    id: u64,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Disconnect(self.id));
    }
}

/// Forwards report requests from other processes to the controller, from a
/// thread of its own that blocks on the request event.
fn listen_for_reports(signal: Arc<ReportSignal>, outbox: WeakUnboundedSender<Event>) {
    std::thread::spawn(move || {
        loop {
            signal.wait();
            let Some(events) = outbox.upgrade() else {
                return;
            };
            if events.send(Event::Report).is_err() {
                return;
            }
        }
    });
}

struct Held {
    release: oneshot::Sender<()>,
    client: Option<Client>,
    since: Instant,
}

struct Open {
    traffic: Arc<Traffic>,
    client: Option<Client>,
    opened: Instant,
}

struct Controller {
    machine: IdleMachine,
    hooks: Arc<dyn EngineHooks>,
    trace: bool,
    outbox: WeakUnboundedSender<Event>,
    /// Replies owed to connections that wait for a start.
    waiting: Vec<oneshot::Sender<Reply>>,
    /// Watchers' connections that wait for someone else to wake the engine.
    held: Vec<Held>,
    /// Counted connections, by ID.
    open: HashMap<u64, Open>,
    /// Bumped by every idle stop: relays of an older epoch close.
    cuts: watch::Sender<u64>,
    report: Option<PathBuf>,
    signal: Option<Arc<ReportSignal>>,
}

impl Controller {
    async fn run(mut self, mut inbox: UnboundedReceiver<Event>) {
        loop {
            let event = match self.machine.deadline() {
                Some(deadline) => {
                    tokio::select! {
                        event = inbox.recv() => event,
                        () = tokio::time::sleep_until(deadline.into()) => {
                            // `max` guarantees the deadline counts as passed,
                            // so the machine moves on and this never spins:
                            // it either checks, or traffic moved the deadline
                            // into the future.
                            let now = Instant::now().max(deadline);
                            self.refresh_traffic();
                            if let Some(action) = self.machine.on_deadline(now) {
                                self.perform(action);
                            }
                            continue;
                        }
                    }
                }
                None => inbox.recv().await,
            };
            let Some(event) = event else {
                return;
            };
            self.handle(event);
        }
    }

    fn handle(&mut self, event: Event) {
        let now = Instant::now();
        let action = match event {
            Event::Connect {
                id,
                client,
                request,
                traffic,
                reply,
            } => self.connect(now, id, client, request, traffic, reply),
            Event::Disconnect(id) => {
                self.open.remove(&id);
                self.machine.disconnect(now, id);
                None
            }
            Event::Started(started) => {
                let proceed = self.machine.start_finished(now, started);
                let epoch = *self.cuts.borrow();
                for reply in self.waiting.drain(..) {
                    let _ = reply.send(if proceed {
                        Reply::Proceed(epoch)
                    } else {
                        Reply::Refused
                    });
                }
                // Held watchers ask again and are let in now. After a failed
                // start they keep waiting for one that works.
                if proceed {
                    for held in self.held.drain(..) {
                        let _ = held.release.send(());
                    }
                }
                None
            }
            Event::Stopped(stopped) => {
                if stopped {
                    // Close what was open when the stop began: those relays
                    // lead into a distro that is gone, and a quiet watcher
                    // would otherwise never learn.
                    self.cuts.send_modify(|epoch| *epoch += 1);
                }
                self.machine.stop_finished(now, stopped)
            }
            Event::Checked(activity) => {
                self.refresh_traffic();
                self.machine.check_finished(now, activity)
            }
            Event::Report => {
                self.write_report(now);
                None
            }
        };
        if let Some(action) = action {
            self.perform(action);
        }
    }

    fn connect(
        &mut self,
        now: Instant,
        id: u64,
        client: Option<Client>,
        request: FirstRequest,
        traffic: Arc<Traffic>,
        reply: oneshot::Sender<Reply>,
    ) -> Option<IdleAction> {
        let client_id = client.as_ref().map(|client| client.id);
        let (admission, action) = self.machine.connect(now, id, client_id, request);
        match admission {
            Admission::Proceed => {
                self.open.insert(id, open(traffic, client, now));
                let _ = reply.send(Reply::Proceed(*self.cuts.borrow()));
            }
            Admission::Wait => {
                self.open.insert(id, open(traffic, client, now));
                self.waiting.push(reply);
            }
            Admission::Hold => {
                if self.trace {
                    eprintln!(
                        "[conn {id}] held: {} only watches; the engine stays asleep",
                        describe(client.as_ref())
                    );
                }
                let (release, released) = oneshot::channel();
                // Entries of connections that gave up or asked again.
                self.held.retain(|held| !held.release.is_closed());
                self.held.push(Held {
                    release,
                    client,
                    since: now,
                });
                let _ = reply.send(Reply::Hold(released));
            }
        }
        action
    }

    /// Tells the machine when each connection last carried data.
    fn refresh_traffic(&mut self) {
        for (id, open) in &self.open {
            self.machine.traffic(*id, open.traffic.last());
        }
    }

    fn write_report(&mut self, now: Instant) {
        self.refresh_traffic();
        if let Some(path) = &self.report {
            let text = self.report(now).render();
            if let Err(err) = std::fs::write(path, text)
                && self.trace
            {
                eprintln!("[engine] report not written to {}: {err}", path.display());
            }
        }
        if let Some(signal) = &self.signal {
            signal.done();
        }
    }

    fn report(&self, now: Instant) -> ProxyReport {
        let mut connections = Vec::new();
        for view in self.machine.connection_views() {
            let Some(open) = self.open.get(&view.id) else {
                continue;
            };
            connections.push(ReportedConnection {
                state: if view.waiting {
                    ConnectionState::Waiting
                } else {
                    ConnectionState::Open
                },
                pid: open.client.as_ref().map(|client| client.id.pid),
                program: open.client.as_ref().and_then(|c| c.program.clone()),
                quiet: now.saturating_duration_since(view.last_traffic),
                age: now.saturating_duration_since(open.opened),
                to_engine: open.traffic.to_engine.load(Ordering::Relaxed),
                to_client: open.traffic.to_client.load(Ordering::Relaxed),
            });
        }
        for held in self.held.iter().filter(|held| !held.release.is_closed()) {
            let waited = now.saturating_duration_since(held.since);
            connections.push(ReportedConnection {
                state: ConnectionState::Held,
                pid: held.client.as_ref().map(|client| client.id.pid),
                program: held.client.as_ref().and_then(|c| c.program.clone()),
                quiet: waited,
                age: waited,
                to_engine: 0,
                to_client: 0,
            });
        }
        ProxyReport {
            phase: self.machine.phase(),
            watchers: self.machine.policy().watchers(),
            idle_after: self.machine.policy().idle_after(),
            unused_for: self
                .machine
                .idle_since()
                .map(|since| now.saturating_duration_since(since)),
            connections,
        }
    }

    /// Runs an engine call on the blocking pool and reports its outcome to
    /// the controller. A call that panics counts as failed, so no connection
    /// waits for a start that will never report back.
    fn perform(&self, action: IdleAction) {
        let Some(outbox) = self.outbox.upgrade() else {
            return;
        };
        let hooks = Arc::clone(&self.hooks);
        let trace = self.trace;
        if trace {
            eprintln!("[engine] {action:?}");
        }
        tokio::task::spawn_blocking(move || {
            let event = match action {
                IdleAction::Start => {
                    Event::Started(succeeded("start", trace, || hooks.start_engine()))
                }
                IdleAction::Stop => {
                    Event::Stopped(succeeded("stop", trace, || hooks.stop_distro()))
                }
                IdleAction::Check => {
                    let activity = catch_unwind(AssertUnwindSafe(|| check_activity(&*hooks)))
                        .unwrap_or(Activity::Unknown);
                    if trace {
                        eprintln!("[engine] check: {activity:?}");
                    }
                    Event::Checked(activity)
                }
            };
            let _ = outbox.send(event);
        });
    }
}

fn open(traffic: Arc<Traffic>, client: Option<Client>, now: Instant) -> Open {
    Open {
        traffic,
        client,
        opened: now,
    }
}

/// `docker.exe (pid 42)`, for trace output.
pub(crate) fn describe(client: Option<&Client>) -> String {
    match client {
        Some(Client {
            id,
            program: Some(program),
        }) => format!("{program} (pid {})", id.pid),
        Some(Client { id, program: None }) => format!("pid {}", id.pid),
        None => "an unknown client".to_string(),
    }
}

fn succeeded(what: &str, trace: bool, call: impl FnOnce() -> std::io::Result<()>) -> bool {
    let outcome = catch_unwind(AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(std::io::Error::other("the engine call panicked")));
    if trace {
        match &outcome {
            Ok(()) => eprintln!("[engine] {what}: done"),
            Err(err) => eprintln!("[engine] {what} failed: {err}"),
        }
    }
    outcome.is_ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use docklet_core::idle::EnginePhase;
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// Engine hooks that only count: a start takes `start_delay`, a stop
    /// stops, nothing ever runs a container.
    #[derive(Default)]
    pub(crate) struct FakeEngine {
        pub running: AtomicBool,
        pub starts: AtomicUsize,
        pub stops: AtomicUsize,
        pub checks: AtomicUsize,
        pub start_delay: Duration,
        /// Starts fail while this is set.
        pub broken: AtomicBool,
        /// When each start began and ended.
        pub start_log: Mutex<Vec<(Instant, Instant)>>,
    }

    impl EngineHooks for FakeEngine {
        fn start_engine(&self) -> io::Result<()> {
            let began = Instant::now();
            self.starts.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.start_delay);
            self.start_log.lock().unwrap().push((began, Instant::now()));
            if self.broken.load(Ordering::SeqCst) {
                return Err(io::Error::other("dockerd did not start"));
            }
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn stop_distro(&self) -> io::Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn distro_running(&self) -> io::Result<bool> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(self.running.load(Ordering::SeqCst))
        }
        fn running_containers(&self) -> io::Result<usize> {
            Ok(0)
        }
    }

    /// Waits until `done` holds, polling every 20 ms, for at most `limit`.
    pub(crate) async fn eventually(limit: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }

    fn idle_stop(engine: &Arc<FakeEngine>) -> IdleStop {
        let policy = IdlePolicy::new(Duration::from_secs(1));
        IdleStop::new(policy, Arc::clone(engine))
    }

    /// No named events: tests must not answer for a proxy that may run on
    /// the same machine.
    fn spawn(idle: IdleStop) -> Lifecycle {
        Lifecycle::spawn_with(idle, false, None)
    }

    fn client(pid: u32, program: &str) -> Client {
        Client {
            id: ClientId {
                pid,
                started: u64::from(pid) * 10,
            },
            program: Some(program.to_string()),
        }
    }

    async fn admit(
        lifecycle: &Lifecycle,
        id: u64,
        client: Option<Client>,
        request: FirstRequest,
    ) -> Admit {
        lifecycle
            .admit(id, client, request, lifecycle.traffic())
            .await
    }

    fn relayed(admit: Admit) -> Option<Admitted> {
        match admit {
            Admit::Relay(admitted) => Some(admitted),
            _ => None,
        }
    }

    /// No pipe and no child process: the controller with fake hooks only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_share_one_start_and_the_idle_engine_stops_once() {
        let engine = Arc::new(FakeEngine {
            start_delay: Duration::from_millis(300),
            ..FakeEngine::default()
        });
        let lifecycle = spawn(idle_stop(&engine));
        let scenario = async {
            let admitted = admit_three(&lifecycle).await;
            assert!(admitted.iter().all(Option::is_some));
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
            assert_eq!(engine.stops.load(Ordering::SeqCst), 0);
            drop(admitted);
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    == 1)
                .await,
                "the idle engine was not stopped"
            );
            // Stopped means quiet: no more checks, no second stop.
            let checks = engine.checks.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(2500)).await;
            assert_eq!(engine.stops.load(Ordering::SeqCst), 1);
            assert_eq!(engine.checks.load(Ordering::SeqCst), checks);
            // The next connection wakes it.
            let next = admit(&lifecycle, 9, None, FirstRequest::Unread).await;
            assert!(relayed(next).is_some());
            assert_eq!(engine.starts.load(Ordering::SeqCst), 2);
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_closes_the_waiters_and_the_next_connection_retries() {
        let engine = Arc::new(FakeEngine {
            start_delay: Duration::from_millis(200),
            ..FakeEngine::default()
        });
        engine.broken.store(true, Ordering::SeqCst);
        let lifecycle = spawn(idle_stop(&engine));
        let scenario = async {
            let admitted = admit_three(&lifecycle).await;
            assert!(admitted.iter().all(Option::is_none));
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
            engine.broken.store(false, Ordering::SeqCst);
            let next = admit(&lifecycle, 9, None, FirstRequest::Unread).await;
            assert!(relayed(next).is_some());
            assert_eq!(engine.starts.load(Ordering::SeqCst), 2);
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_relay_still_counts_its_connection_out() {
        let engine = Arc::new(FakeEngine::default());
        // Only with watchers kept awake does an open connection hold the
        // engine up, so only there can a lost guard keep it up forever.
        let lifecycle = spawn(IdleStop::new(
            IdlePolicy::new(Duration::from_secs(1))
                .with_watchers(docklet_core::idle::Watchers::KeepAwake),
            Arc::clone(&engine),
        ));
        let scenario = async {
            let task = tokio::spawn({
                let lifecycle = lifecycle.clone();
                async move {
                    let _admitted = relayed(admit(&lifecycle, 1, None, FirstRequest::Unread).await)
                        .expect("admitted");
                    panic!("relay failed");
                }
            });
            assert!(task.await.unwrap_err().is_panic());
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    >= 1)
                .await,
                "the panicked connection kept the engine in use"
            );
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    /// The case seen with Docker Desktop: a watcher holds a connection open
    /// and reconnects right after it is closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quiet_watcher_is_cut_at_the_stop_and_its_reconnect_waits_for_someone_else() {
        let engine = Arc::new(FakeEngine::default());
        let lifecycle = spawn(idle_stop(&engine));
        let watcher = client(4242, "com.docker.build.exe");
        let scenario = async {
            let first =
                relayed(admit(&lifecycle, 1, Some(watcher.clone()), FirstRequest::Unread).await)
                    .expect("the first connection is relayed");
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
            // It stays open and says nothing: the engine stops anyway, and
            // the connection is told to close.
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    == 1)
                .await,
                "a quiet connection kept the engine awake"
            );
            tokio::time::timeout(Duration::from_secs(5), first.cut.wait())
                .await
                .expect("the quiet connection was not cut");
            drop(first.guard);
            // The watcher reconnects and subscribes again: held, no start.
            let Admit::Hold(release) =
                admit(&lifecycle, 2, Some(watcher.clone()), FirstRequest::Unread).await
            else {
                panic!("the reconnect was not held");
            };
            let Admit::Hold(release) = (tokio::select! {
                _ = release => panic!("released without a wake"),
                again = admit(&lifecycle, 2, Some(watcher.clone()), FirstRequest::Passive) => again,
            }) else {
                panic!("the subscription was not held");
            };
            tokio::time::sleep(Duration::from_millis(2500)).await;
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1, "a watcher woke it");
            // Someone else wakes it; the watcher is let in after the start.
            let other = relayed(
                admit(
                    &lifecycle,
                    3,
                    Some(client(77, "docker.exe")),
                    FirstRequest::Unread,
                )
                .await,
            )
            .expect("another client is relayed");
            assert_eq!(engine.starts.load(Ordering::SeqCst), 2);
            tokio::time::timeout(Duration::from_secs(5), release)
                .await
                .expect("the held connection was not released")
                .expect("released, not dropped");
            let again = admit(&lifecycle, 2, Some(watcher), FirstRequest::Passive).await;
            assert!(
                relayed(again).is_some(),
                "the watcher is relayed after the wake"
            );
            drop(other);
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn traffic_keeps_the_engine_awake_and_silence_lets_it_sleep() {
        let engine = Arc::new(FakeEngine::default());
        let lifecycle = spawn(idle_stop(&engine));
        let scenario = async {
            let traffic = lifecycle.traffic();
            let busy = relayed(
                lifecycle
                    .admit(1, None, FirstRequest::Unread, Arc::clone(&traffic))
                    .await,
            )
            .expect("relayed");
            // Three times the idle time, with data every 200 ms.
            for _ in 0..15 {
                traffic.to_client(100);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            assert_eq!(engine.stops.load(Ordering::SeqCst), 0, "stopped while busy");
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    == 1)
                .await,
                "the engine did not stop once the data ended"
            );
            drop(busy);
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_report_names_the_connections_and_their_clients() {
        let engine = Arc::new(FakeEngine::default());
        let path =
            std::env::temp_dir().join(format!("docklet-proxy-report-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let idle = IdleStop::new(
            IdlePolicy::new(Duration::from_secs(600)),
            Arc::clone(&engine),
        )
        .with_report(path.clone());
        let lifecycle = spawn(idle);
        let scenario = async {
            let traffic = lifecycle.traffic();
            let watcher = client(4242, "com.docker.build.exe");
            let admitted = relayed(
                lifecycle
                    .admit(1, Some(watcher), FirstRequest::Unread, Arc::clone(&traffic))
                    .await,
            )
            .expect("relayed");
            traffic.to_engine(120);
            traffic.to_client(3400);
            lifecycle.request_report();
            assert!(
                eventually(Duration::from_secs(10), || path.is_file()).await,
                "no report written"
            );
            let report = ProxyReport::parse(&std::fs::read_to_string(&path).unwrap())
                .expect("the report reads back");
            assert_eq!(report.phase, EnginePhase::Running);
            assert_eq!(report.idle_after, Duration::from_secs(600));
            assert_eq!(report.connections.len(), 1);
            let connection = &report.connections[0];
            assert_eq!(connection.state, ConnectionState::Open);
            assert_eq!(connection.pid, Some(4242));
            assert_eq!(connection.program.as_deref(), Some("com.docker.build.exe"));
            assert_eq!((connection.to_engine, connection.to_client), (120, 3400));
            drop(admitted);
        };
        let outcome = tokio::time::timeout(Duration::from_secs(30), scenario).await;
        let _ = std::fs::remove_file(&path);
        outcome.expect("the scenario did not finish within 30 s");
    }

    #[test]
    fn traffic_remembers_the_latest_data() {
        let base = Instant::now();
        let traffic = Traffic::new(base);
        let first = traffic.last();
        assert!(first >= base);
        std::thread::sleep(Duration::from_millis(5));
        traffic.to_engine(7);
        traffic.to_client(9);
        assert!(traffic.last() > first);
        assert_eq!(traffic.to_engine.load(Ordering::Relaxed), 7);
        assert_eq!(traffic.to_client.load(Ordering::Relaxed), 9);
        assert_eq!(describe(None), "an unknown client");
        assert_eq!(describe(Some(&client(5, "x.exe"))), "x.exe (pid 5)");
    }

    /// Three connections arriving together.
    async fn admit_three(lifecycle: &Lifecycle) -> [Option<Admitted>; 3] {
        let unread = FirstRequest::Unread;
        let (a, b, c) = tokio::join!(
            admit(lifecycle, 0, None, unread),
            admit(lifecycle, 1, None, unread),
            admit(lifecycle, 2, None, unread)
        );
        [relayed(a), relayed(b), relayed(c)]
    }
}
