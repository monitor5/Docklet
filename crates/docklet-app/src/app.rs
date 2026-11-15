//! The whole application behind one handle, for a shell with a message
//! loop: requests go in as method calls that return at once, results come
//! back as [`Message`]s.
//!
//! The shell's part is a [`Sink`] that posts each message to its UI thread
//! (`PostMessageW` with the message boxed in `LPARAM`), and a handler there
//! that feeds the models: `ListModel`, `UsageModel`, `LogBuffer`,
//! `PullProgress`.
//!
//! Nothing in here talks to Docker unless the last status poll said the
//! engine runs. That is what keeps a sleeping engine asleep: a list, an
//! action or a log asked for in any other state is turned away here, not
//! tried.

use std::future::Future;
use std::io;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use docklet_core::EngineState;
use docklet_engine::models::{ContainerSummary, ImageSummary, Network, Volume};
use docklet_engine::{
    Client, ContainerAction, ContainerUsage, DiskUsage, Error, PruneReport, RemoveReport,
    ResourceKind,
};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::backend::Backend;
use crate::control::{EngineHost, EngineStatus, SystemHost, Transition};
use crate::policy::{
    EVENT_DEBOUNCE, WindowState, next_poll_delay, reloads_lists, repolls_status, sampling,
    uses_docker_api,
};
use crate::pull::{ReferenceError, check_reference};
use crate::removal::{self, PruneTarget, RemovalPlan};
use crate::streams::{DockerEvent, HostReading, LogMessage, StreamSink};
use crate::table::{Table, containers_table, images_table, networks_table, volumes_table};

/// What a Docker request is answered with when the engine does not run.
const NOT_RUNNING: &str = "the engine is not running";

/// One of the four lists, as the engine returned it.
#[derive(Debug, Clone, PartialEq)]
pub enum List {
    Containers(Vec<ContainerSummary>),
    Images(Vec<ImageSummary>),
    Volumes(Vec<Volume>),
    Networks(Vec<Network>),
}

impl List {
    pub fn kind(&self) -> ResourceKind {
        match self {
            Self::Containers(_) => ResourceKind::Container,
            Self::Images(_) => ResourceKind::Image,
            Self::Volumes(_) => ResourceKind::Volume,
            Self::Networks(_) => ResourceKind::Network,
        }
    }

    /// The list as a table. `usage` fills the usage columns of containers,
    /// `disk` the sizes of volumes; pass what `UsageModel` has.
    pub fn table(&self, usage: Option<&[ContainerUsage]>, disk: Option<&DiskUsage>) -> Table {
        match self {
            Self::Containers(containers) => containers_table(containers, usage),
            Self::Images(images) => images_table(images),
            Self::Volumes(volumes) => volumes_table(volumes, disk),
            Self::Networks(networks) => networks_table(networks),
        }
    }
}

/// What the app tells the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The answer to a status poll. An error leaves the state as it was.
    Status(Result<EngineStatus, String>),
    /// One step of a running install.
    InstallProgress(String),
    /// An install, start or stop is over. `error` is `None` when it worked;
    /// a `Status` follows either way.
    EngineRequestDone {
        transition: Transition,
        error: Option<String>,
    },
    /// The event stream said something. The lists reload by themselves;
    /// this is for a shell that wants to show the connection.
    Docker(DockerEvent),
    /// A list was loaded, because it was asked for or because Docker
    /// reported a change.
    List {
        kind: ResourceKind,
        result: Result<List, String>,
    },
    /// A start, stop, restart or removal of one container is over.
    ActionDone {
        id: String,
        action: ContainerAction,
        error: Option<String>,
    },
    /// A bulk removal is over; `removal::removal_summary` puts it in words.
    Removed {
        plan: RemovalPlan,
        report: RemoveReport,
    },
    /// A prune is over; `removal::prune_summary` puts it in words.
    Pruned {
        target: PruneTarget,
        result: Result<PruneReport, String>,
    },
    /// One progress line of a running pull.
    PullProgress(String),
    PullDone {
        reference: String,
        error: Option<String>,
    },
    /// Lines of a followed log, or its end. `token` is what `follow_logs`
    /// returned.
    Log { token: u32, message: LogMessage },
    /// The host's figures, every two seconds while they are sampled.
    HostUsage(HostReading),
    /// The usage of the running containers, about once a second while it is
    /// sampled; `None` when it stopped coming.
    ContainerUsage(Option<Vec<ContainerUsage>>),
    /// Docker's disk usage, which was asked for.
    DiskUsage(Result<DiskUsage, String>),
}

/// Where the app delivers. `post` is called on worker threads, and now and
/// then on the thread that made a request, before the request returns. It
/// must not block and must not call back into the `App`: hand the message
/// to the UI thread and return.
pub trait Sink: Send + Sync + 'static {
    fn post(&self, message: Message);
}

impl<F: Fn(Message) + Send + Sync + 'static> Sink for F {
    fn post(&self, message: Message) {
        self(message);
    }
}

/// The application. Every method returns at once and may be called from any
/// thread; the UI thread is the usual one.
///
/// Dropping it ends everything it started. Engine work in flight (an install
/// that is downloading) is not waited for.
pub struct App {
    inner: Arc<Inner>,
    poll: JoinHandle<()>,
    /// The runtime, when the app made its own.
    runtime: Option<Runtime>,
}

struct Inner {
    backend: Backend,
    runtime: Handle,
    sink: Arc<dyn Sink>,
    /// Asks the poll task for a poll now.
    poll_now: Notify,
    /// Tells the poll task that its delay may have changed.
    rearm: Notify,
    shared: Mutex<Shared>,
    /// Starts and stops of this app in flight.
    acting: AtomicUsize,
    /// Counts the events that ask for a reload, so only the last of a burst
    /// reloads.
    reloads: AtomicU64,
    /// Counts the beginnings and ends of stops, changed under the `shared`
    /// lock. A poll that saw it change does not know which engine it saw.
    stops: AtomicU64,
}

#[derive(Default)]
struct Shared {
    window: WindowState,
    /// The state the last poll answered with.
    state: Option<EngineState>,
    /// The list the window shows, which reloads on Docker's events.
    list: Option<ResourceKind>,
    /// The window shows usage figures.
    shows_usage: bool,
}

impl App {
    /// The app for this user's real engine, on a small runtime of its own.
    /// `sink` receives everything; see [`Sink`] for the threads.
    pub fn new(sink: Arc<dyn Sink>) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("docklet-app")
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let mut app = Self::with_host(handle, Arc::new(SystemHost::new()), sink);
        app.runtime = Some(runtime);
        Ok(app)
    }

    /// The app for the engine on `host`, on a runtime the caller keeps
    /// alive. `SystemHost::with_proxy_command` is the host for a shell that
    /// has no `--proxy` mode of its own.
    pub fn with_host(runtime: Handle, host: Arc<dyn EngineHost>, sink: Arc<dyn Sink>) -> Self {
        let inner = Arc::new_cyclic(|inner: &Weak<Inner>| Inner {
            backend: Backend::new(host, runtime.clone(), Arc::new(Relay(inner.clone()))),
            runtime: runtime.clone(),
            sink,
            poll_now: Notify::new(),
            rearm: Notify::new(),
            shared: Mutex::default(),
            acting: AtomicUsize::new(0),
            reloads: AtomicU64::new(0),
            stops: AtomicU64::new(0),
        });
        let poll = runtime.spawn(poll_engine(inner.clone()));
        Self {
            inner,
            poll,
            runtime: None,
        }
    }

    /// Says what became of the window. Call it when the window is created
    /// or shown (`Visible`), minimised or hidden (`Hidden`) and destroyed
    /// (`Gone`).
    ///
    /// The engine state is polled only while the window is visible, and
    /// once when it becomes visible. `Hidden` ends the usage sampling;
    /// `Gone` also ends the event subscription and every followed log.
    /// Saying the same state again does nothing, so it can be called from
    /// every message that might have changed it.
    pub fn set_window(&self, window: WindowState) {
        {
            let mut shared = self.inner.shared();
            if shared.window == window {
                return;
            }
            shared.window = window;
            if window == WindowState::Gone {
                shared.list = None;
            }
        }
        self.inner.backend.streams().set_window(window);
        self.inner.apply_sampling();
        if window == WindowState::Visible {
            self.inner.poll_now.notify_one();
        } else {
            self.inner.rearm.notify_one();
        }
    }

    /// True while a start or a stop asked for here is in flight: the time
    /// to disable the Start and Stop buttons, since the engine would turn a
    /// second request away.
    pub fn is_acting(&self) -> bool {
        self.inner.acting.load(Ordering::Relaxed) > 0
    }

    /// Looks at the engine state now. A `Status` follows.
    pub fn refresh_status(&self) {
        self.inner.poll_now.notify_one();
    }

    /// Installs the distro from the published rootfs for `base`, or for the
    /// default base. `InstallProgress` messages follow, then
    /// `EngineRequestDone` and a `Status`.
    pub fn install(&self, base: Option<String>) {
        self.engine_request(Transition::Installing, move |inner| {
            let mut progress =
                |step: &str| inner.sink.post(Message::InstallProgress(step.to_string()));
            inner.backend.install(base, &mut progress)
        });
    }

    /// Starts the engine, or wakes it. `EngineRequestDone` and a `Status`
    /// follow. A request while another one runs is turned away with an
    /// error that says which.
    pub fn start(&self) {
        self.engine_request(Transition::Starting, |inner| inner.backend.start());
    }

    /// Stops the engine and its pipe proxy. `EngineRequestDone` and a
    /// `Status` follow.
    pub fn stop(&self) {
        // From here on nothing asks Docker for anything, not even a reload
        // that an event from before the stop still has pending. The poll
        // that follows the stop says what the engine is then.
        {
            let mut shared = self.inner.shared();
            self.inner.stops.fetch_add(1, Ordering::Relaxed);
            shared.state = Some(EngineState::Stopping);
        }
        self.inner.apply_sampling();
        self.engine_request(Transition::Stopping, |inner| {
            let outcome = inner.backend.stop();
            let _shared = inner.shared();
            inner.stops.fetch_add(1, Ordering::Relaxed);
            outcome
        });
    }

    fn engine_request(
        &self,
        transition: Transition,
        work: impl FnOnce(&Inner) -> io::Result<()> + Send + 'static,
    ) {
        let inner = self.inner.clone();
        // An install shows its own progress; a start or a stop is what the
        // fast poll is for.
        let acts = transition != Transition::Installing;
        if acts {
            inner.acting.fetch_add(1, Ordering::Relaxed);
            inner.rearm.notify_one();
        }
        self.inner.runtime.spawn_blocking(move || {
            let error = work(&inner).err().map(|err| err.to_string());
            if acts {
                inner.acting.fetch_sub(1, Ordering::Relaxed);
            }
            inner
                .sink
                .post(Message::EngineRequestDone { transition, error });
            inner.poll_now.notify_one();
        });
    }

    /// Says which list the window shows, or `None` for none. The list is
    /// loaded now and again whenever Docker reports a change; each load
    /// arrives as a `List`. Does nothing unless the engine runs.
    pub fn show_list(&self, kind: Option<ResourceKind>) {
        self.inner.shared().list = kind;
        let Some(kind) = kind else { return };
        if self.inner.may_use_docker() {
            self.inner.backend.streams().watch_events();
        }
        self.reload(kind);
    }

    /// Loads a list now. A `List` follows, unless the engine does not run.
    pub fn reload(&self, kind: ResourceKind) {
        let inner = self.inner.clone();
        self.spawn(async move { inner.load(kind).await });
    }

    /// Starts, stops, restarts or removes one container. `ActionDone`
    /// follows; the list reloads by itself when Docker reports the change.
    pub fn container_action(&self, id: String, action: ContainerAction) {
        let inner = self.inner.clone();
        self.spawn(async move {
            let outcome = match inner.client() {
                Ok(client) => client.container_action(&id, action).await.map_err(text),
                Err(error) => Err(error),
            };
            let error = outcome.err();
            inner.sink.post(Message::ActionDone { id, action, error });
        });
    }

    /// Removes everything in the plan. `Removed` follows, with a result for
    /// every item.
    pub fn remove(&self, plan: RemovalPlan) {
        let inner = self.inner.clone();
        self.spawn(async move {
            let report = match inner.client() {
                Ok(client) => removal::remove(&client, &plan).await,
                Err(error) => removal::not_removed(&plan, &error),
            };
            inner.sink.post(Message::Removed { plan, report });
        });
    }

    /// Removes everything unused of one kind. `Pruned` follows.
    pub fn prune(&self, target: PruneTarget) {
        let inner = self.inner.clone();
        self.spawn(async move {
            let result = match inner.client() {
                Ok(client) => removal::prune(&client, target).await.map_err(text),
                Err(error) => Err(error),
            };
            inner.sink.post(Message::Pruned { target, result });
        });
    }

    /// Pulls an image given the way `docker pull` takes it. `PullProgress`
    /// messages follow, then `PullDone`. Nothing follows when nothing was
    /// typed.
    pub fn pull(&self, typed: &str) {
        let reference = match check_reference(typed) {
            Ok(reference) => reference,
            Err(ReferenceError::Empty) => return,
            Err(ReferenceError::Invalid(error)) => {
                let reference = typed.trim().to_string();
                let error = Some(error);
                return self.inner.sink.post(Message::PullDone { reference, error });
            }
        };
        let inner = self.inner.clone();
        self.spawn(async move {
            let progress = |line: String| inner.sink.post(Message::PullProgress(line));
            let outcome = match inner.client() {
                Ok(client) => client.pull_image(&reference, progress).await.map_err(text),
                Err(error) => Err(error),
            };
            let error = outcome.err();
            inner.sink.post(Message::PullDone { reference, error });
        });
    }

    /// Follows a container's logs, starting with its last 500 lines. `Log`
    /// messages with the returned token follow; the last one is `Ended`,
    /// unless the log is stopped from here.
    pub fn follow_logs(&self, id: String) -> u32 {
        let streams = self.inner.backend.streams();
        let token = streams.log_token();
        let sink = self.inner.sink.clone();
        let output = move |message: LogMessage| {
            sink.post(Message::Log { token, message });
            ControlFlow::Continue(())
        };
        match self.inner.client() {
            Ok(client) => streams.follow_logs_as(token, client, id, output),
            Err(error) => {
                let error = Some(error);
                let _ = output(LogMessage::Ended { error });
            }
        }
        token
    }

    /// Stops a followed log. No `Ended` follows.
    pub fn stop_logs(&self, token: u32) {
        self.inner.backend.streams().stop_logs(token);
    }

    /// Says whether the window shows usage figures. While it does and is
    /// visible, `HostUsage` arrives every two seconds and, from a running
    /// engine whose list is shown, `ContainerUsage` about once a second.
    pub fn set_usage(&self, shown: bool) {
        self.inner.shared().shows_usage = shown;
        self.inner.apply_sampling();
    }

    /// Works out Docker's disk usage, which takes the engine seconds. Call
    /// it when the view that shows it opens and when the user asks, never
    /// on a timer (see `policy::loads_disk_usage`). `DiskUsage` follows.
    pub fn load_disk_usage(&self) {
        let inner = self.inner.clone();
        self.spawn(async move {
            let result = match inner.client() {
                Ok(client) => client.disk_usage().await.map_err(text),
                Err(error) => Err(error),
            };
            inner.sink.post(Message::DiskUsage(result));
        });
    }

    fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        self.inner.runtime.spawn(work);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.poll.abort();
        self.inner.backend.streams().set_window(WindowState::Gone);
        if let Some(runtime) = self.runtime.take() {
            // Not waited for: an install in flight would hold up the exit.
            runtime.shutdown_background();
        }
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Inner {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        // Plain values: still good after a panic elsewhere poisoned the
        // lock.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether Docker may be asked: only when the last poll said the engine
    /// runs. A call in any other state fails, or wakes a sleeping engine.
    fn may_use_docker(&self) -> bool {
        self.shared().state.as_ref().is_some_and(uses_docker_api)
    }

    /// A client, if Docker may be asked; otherwise why not.
    fn client(&self) -> Result<Client, String> {
        if !self.may_use_docker() {
            return Err(NOT_RUNNING.to_string());
        }
        Client::connect().map_err(text)
    }

    fn next_poll_delay(&self) -> Option<Duration> {
        let shared = self.shared();
        let visible = shared.window == WindowState::Visible;
        let busy = self.acting.load(Ordering::Relaxed) > 0;
        next_poll_delay(shared.state.as_ref(), visible, busy)
    }

    /// Tells the streams what to sample, after the window, the engine state
    /// or what the window shows changed (see `policy::sampling`).
    fn apply_sampling(&self) {
        // Under the lock, so two changes are applied in the order they
        // were made.
        let shared = self.shared();
        let sampled = sampling(shared.window, shared.state.as_ref());
        let shown = shared.shows_usage;
        let streams = self.backend.streams();
        streams.set_usage(shown && sampled.host, shown && sampled.containers);
    }

    /// Polls the engine state once and tells the shell.
    async fn poll(self: &Arc<Self>) {
        let stops = self.stops.load(Ordering::Relaxed);
        let inner = self.clone();
        let status = self.runtime.spawn_blocking(move || inner.backend.status());
        let result = match status.await {
            Ok(status) => status.map_err(text),
            Err(panic) => Err(panic.to_string()),
        };
        {
            let mut shared = self.shared();
            // A stop began or ended while the engine was looked at, so what
            // was seen may be the engine from before it. Saying `Running`
            // now would let requests through to an engine that is being
            // stopped. The end of the stop asks for another poll.
            if self.stops.load(Ordering::Relaxed) != stops {
                return;
            }
            if let Ok(status) = &result {
                shared.state = Some(status.state.clone());
            }
        }
        self.apply_sampling();
        self.sink.post(Message::Status(result));
    }

    async fn load(&self, kind: ResourceKind) {
        // Not even an error: the lists are not shown in this state.
        if !self.may_use_docker() {
            return;
        }
        let list = async {
            let client = Client::connect()?;
            Ok::<_, Error>(match kind {
                ResourceKind::Container => List::Containers(client.containers().await?),
                ResourceKind::Image => List::Images(client.images().await?),
                ResourceKind::Volume => List::Volumes(client.volumes().await?),
                ResourceKind::Network => List::Networks(client.networks().await?),
            })
        };
        let result = list.await.map_err(text);
        self.sink.post(Message::List { kind, result });
    }

    /// Reloads the shown list a moment from now, unless another event asks
    /// for it again before that: a burst of events costs one reload.
    fn reload_soon(self: &Arc<Self>) {
        let turn = self.reloads.fetch_add(1, Ordering::Relaxed) + 1;
        let inner = self.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(EVENT_DEBOUNCE).await;
            if inner.reloads.load(Ordering::Relaxed) != turn {
                return;
            }
            let shown = inner.shared().list;
            if let Some(kind) = shown {
                inner.load(kind).await;
            }
        });
    }
}

/// Polls the engine state for as long as the app lives: when asked to, and
/// on the timer of `policy::next_poll_delay` while the window is visible.
///
/// Every round waits: for the timer (at least four seconds), or for somebody
/// to ask (`poll_now`, `rearm`), which only happens on a request of the
/// shell, at the end of an engine request, or when the event stream breaks
/// or returns, which its backoff keeps at least a second apart.
async fn poll_engine(inner: Arc<Inner>) {
    loop {
        let timer = async {
            match inner.next_poll_delay() {
                Some(delay) => tokio::time::sleep(delay).await,
                // Not visible: no poll until somebody asks.
                None => std::future::pending().await,
            }
        };
        let poll = tokio::select! {
            () = inner.poll_now.notified() => true,
            () = timer => true,
            // The window or a request changed the delay: only work it out
            // again.
            () = inner.rearm.notified() => false,
        };
        if poll {
            inner.poll().await;
        }
    }
}

/// Hands what the streams deliver to the shell, and does what follows from
/// an event: another look at the engine, a reload of the shown list.
struct Relay(Weak<Inner>);

impl StreamSink for Relay {
    fn docker_event(&self, event: DockerEvent) {
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        if repolls_status(&event) {
            inner.poll_now.notify_one();
        }
        if reloads_lists(&event) {
            inner.reload_soon();
        }
        inner.sink.post(Message::Docker(event));
    }

    fn container_usage(&self, usage: Option<Vec<ContainerUsage>>) {
        if let Some(inner) = self.0.upgrade() {
            inner.sink.post(Message::ContainerUsage(usage));
        }
    }

    fn host_usage(&self, reading: HostReading) {
        if let Some(inner) = self.0.upgrade() {
            inner.sink.post(Message::HostUsage(reading));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use docklet_core::ProxyMode;
    use docklet_engine::RemoveFailure;

    use super::*;
    use crate::control::testing::FakeHost;
    use crate::table::testing::{container, image};

    /// Longer than any of these tests should take; none of them waits for
    /// it unless it fails.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// How long nothing must arrive for a test to call it "nothing".
    const QUIET: Duration = Duration::from_millis(300);

    struct Harness {
        app: App,
        host: Arc<FakeHost>,
        messages: Receiver<Message>,
        _runtime: Runtime,
    }

    /// An app on a fake engine. None of these tests lets the engine be
    /// both reachable and watched by a window, so nothing ever opens the
    /// real engine pipe.
    fn harness(state: EngineState, proxy: ProxyMode) -> Harness {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .unwrap();
        let host = FakeHost::new(state, proxy);
        let (sender, messages) = mpsc::channel();
        let sender = Mutex::new(sender);
        let sink = move |message: Message| {
            let _ = sender.lock().unwrap().send(message);
        };
        let app = App::with_host(runtime.handle().clone(), host.clone(), Arc::new(sink));
        Harness {
            app,
            host,
            messages,
            _runtime: runtime,
        }
    }

    impl Harness {
        fn next(&self) -> Message {
            self.messages.recv_timeout(TIMEOUT).expect("a message")
        }

        fn status(&self) -> EngineState {
            match self.next() {
                Message::Status(Ok(status)) => status.state,
                other => panic!("expected a status, got {other:?}"),
            }
        }

        fn nothing_arrives(&self) -> bool {
            self.messages.recv_timeout(QUIET).is_err()
        }
    }

    #[test]
    fn the_state_is_polled_when_the_window_becomes_visible() {
        let harness = harness(EngineState::NotInstalled, ProxyMode::Down);
        assert!(harness.nothing_arrives());
        harness.app.set_window(WindowState::Visible);
        assert_eq!(harness.status(), EngineState::NotInstalled);
        // The next poll is seconds away.
        assert!(harness.nothing_arrives());
    }

    #[test]
    fn saying_the_same_window_state_again_polls_nothing() {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        harness.app.set_window(WindowState::Visible);
        assert_eq!(harness.status(), EngineState::Stopped);
        for _ in 0..20 {
            harness.app.set_window(WindowState::Visible);
        }
        assert!(harness.nothing_arrives());
    }

    #[test]
    fn a_hidden_window_is_only_polled_on_request() {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        harness.app.set_window(WindowState::Hidden);
        assert!(harness.nothing_arrives());
        harness.app.refresh_status();
        assert_eq!(harness.status(), EngineState::Stopped);
    }

    #[test]
    fn a_start_reports_its_end_and_then_the_new_state() {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        harness.app.start();
        assert_eq!(
            harness.next(),
            Message::EngineRequestDone {
                transition: Transition::Starting,
                error: None,
            }
        );
        assert_eq!(harness.status(), EngineState::Running);
        assert_eq!(harness.host.calls(), ["start_distro", "spawn_proxy"]);
    }

    #[test]
    fn a_start_that_fails_says_why() {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        *harness.host.start_result.lock().unwrap() = Some("dockerd did not answer".to_string());
        harness.app.start();
        assert_eq!(
            harness.next(),
            Message::EngineRequestDone {
                transition: Transition::Starting,
                error: Some("dockerd did not answer".to_string()),
            }
        );
        assert_eq!(harness.status(), EngineState::Stopped);
    }

    #[test]
    fn an_install_reports_each_step() {
        let harness = harness(EngineState::NotInstalled, ProxyMode::Down);
        harness.app.install(Some("alpine".to_string()));
        assert_eq!(
            harness.next(),
            Message::InstallProgress("Installing alpine".to_string())
        );
        assert_eq!(
            harness.next(),
            Message::EngineRequestDone {
                transition: Transition::Installing,
                error: None,
            }
        );
        assert_eq!(harness.status(), EngineState::Stopped);
    }

    #[test]
    fn a_stop_reports_its_end_and_then_the_new_state() {
        let harness = harness(EngineState::Running, ProxyMode::IdleStop);
        harness.app.stop();
        assert_eq!(
            harness.next(),
            Message::EngineRequestDone {
                transition: Transition::Stopping,
                error: None,
            }
        );
        assert_eq!(harness.status(), EngineState::Stopped);
        assert_eq!(harness.host.calls(), ["stop_proxy", "stop_distro"]);
    }

    /// An app that knows its engine sleeps.
    fn asleep() -> Harness {
        let harness = harness(EngineState::Stopped, ProxyMode::IdleStop);
        harness.app.refresh_status();
        assert_eq!(harness.status(), EngineState::Sleeping);
        harness
    }

    #[test]
    fn a_sleeping_engine_is_not_asked_for_its_lists() {
        let harness = asleep();
        harness.app.set_window(WindowState::Hidden);
        harness.app.show_list(Some(ResourceKind::Container));
        harness.app.reload(ResourceKind::Image);
        assert!(harness.nothing_arrives());
        assert_eq!(harness.host.calls(), [] as [&str; 0]);
    }

    #[test]
    fn requests_to_a_sleeping_engine_are_turned_away_not_tried() {
        let harness = asleep();
        harness
            .app
            .container_action("abc".to_string(), ContainerAction::Start);
        match harness.next() {
            Message::ActionDone { id, action, error } => {
                assert_eq!((id.as_str(), action), ("abc", ContainerAction::Start));
                assert_eq!(error.as_deref(), Some(NOT_RUNNING));
            }
            other => panic!("expected the action's end, got {other:?}"),
        }

        harness.app.prune(PruneTarget::StoppedContainers);
        assert_eq!(
            harness.next(),
            Message::Pruned {
                target: PruneTarget::StoppedContainers,
                result: Err(NOT_RUNNING.to_string()),
            }
        );

        harness.app.load_disk_usage();
        assert_eq!(
            harness.next(),
            Message::DiskUsage(Err(NOT_RUNNING.to_string()))
        );

        harness.app.pull("nginx:latest");
        assert_eq!(
            harness.next(),
            Message::PullDone {
                reference: "nginx:latest".to_string(),
                error: Some(NOT_RUNNING.to_string()),
            }
        );

        let token = harness.app.follow_logs("abc".to_string());
        let error = Some(NOT_RUNNING.to_string());
        let message = LogMessage::Ended { error };
        assert_eq!(harness.next(), Message::Log { token, message });
        // Nothing of this woke the engine.
        assert_eq!(harness.host.calls(), [] as [&str; 0]);
    }

    #[test]
    fn a_removal_from_a_sleeping_engine_fails_every_item() {
        let harness = asleep();
        let table = containers_table(&[container("a", None, true)], None);
        let plan = RemovalPlan::new(ResourceKind::Container, &table.rows);
        harness.app.remove(plan.clone());
        match harness.next() {
            Message::Removed { plan: sent, report } => {
                assert_eq!(sent, plan);
                assert_eq!(report.count(RemoveFailure::Other), 1);
                assert_eq!(
                    removal::removal_summary(&sent, &report),
                    "Nothing was removed. 1 failed: a. The engine said: the engine is not running"
                );
            }
            other => panic!("expected the removal's end, got {other:?}"),
        }
    }

    #[test]
    fn a_stop_shuts_docker_off_before_the_poll_says_so() {
        let harness = harness(EngineState::Running, ProxyMode::Plain);
        harness.app.refresh_status();
        assert_eq!(harness.status(), EngineState::Running);
        harness.app.stop();
        // Whatever the stop is doing right now, no request reaches Docker.
        harness.app.load_disk_usage();
        let mut refused = false;
        for _ in 0..3 {
            refused |= harness.next() == Message::DiskUsage(Err(NOT_RUNNING.to_string()));
        }
        assert!(refused);
    }

    /// An app whose next start takes until the returned sender says so.
    fn slow_start() -> (Harness, mpsc::Sender<()>) {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        let (finish, gate) = mpsc::channel();
        *harness.host.start_gate.lock().unwrap() = Some(gate);
        (harness, finish)
    }

    #[test]
    fn a_request_in_flight_shows_as_acting_until_it_is_over() {
        let (harness, finish) = slow_start();
        assert!(!harness.app.is_acting());
        harness.app.start();
        assert!(harness.app.is_acting());
        finish.send(()).unwrap();
        assert!(matches!(harness.next(), Message::EngineRequestDone { .. }));
        assert!(!harness.app.is_acting());
    }

    #[test]
    fn a_stop_during_a_start_is_turned_away_and_the_start_goes_on() {
        let (harness, finish) = slow_start();
        harness.app.start();
        // The tray and the window asking at the same moment. The start is
        // in the distro's hands before the stop gets its turn.
        let deadline = std::time::Instant::now() + TIMEOUT;
        while !harness.host.calls().contains(&"start_distro") {
            assert!(std::time::Instant::now() < deadline, "the start never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        harness.app.stop();
        assert_eq!(
            harness.next(),
            Message::EngineRequestDone {
                transition: Transition::Stopping,
                error: Some("the engine is already starting".to_string()),
            }
        );
        finish.send(()).unwrap();
        let started = Message::EngineRequestDone {
            transition: Transition::Starting,
            error: None,
        };
        // A status may come first: the refused stop asked for one.
        let mut next = harness.next();
        while matches!(next, Message::Status(_)) {
            next = harness.next();
        }
        assert_eq!(next, started);
        assert_eq!(harness.host.calls(), ["start_distro", "spawn_proxy"]);
    }

    #[test]
    fn an_install_is_not_what_the_fast_poll_is_for() {
        let harness = harness(EngineState::NotInstalled, ProxyMode::Down);
        harness.app.install(None);
        assert!(!harness.app.is_acting());
    }

    #[test]
    fn what_was_typed_is_checked_before_anything_is_pulled() {
        let harness = harness(EngineState::Stopped, ProxyMode::Down);
        harness.app.pull("   ");
        assert!(harness.nothing_arrives());
        harness.app.pull(" nginx: ");
        assert_eq!(
            harness.next(),
            Message::PullDone {
                reference: "nginx:".to_string(),
                error: Some("invalid image reference `nginx:`".to_string()),
            }
        );
    }

    #[test]
    fn a_list_becomes_the_table_of_its_kind() {
        let containers = List::Containers(vec![container("web", None, true)]);
        assert_eq!(containers.kind(), ResourceKind::Container);
        assert_eq!(containers.table(None, None).rows[0].name, "web");
        let images = List::Images(vec![image("sha256:abc", &["a:1"], 1, 0)]);
        assert_eq!(images.kind(), ResourceKind::Image);
        assert_eq!(images.table(None, None).kind, ResourceKind::Image);
        assert_eq!(List::Volumes(Vec::new()).kind(), ResourceKind::Volume);
        assert_eq!(List::Networks(Vec::new()).kind(), ResourceKind::Network);
    }
}
