//! The long-lived work that feeds the UI: Docker's event stream, followed
//! logs, container stats and the host's usage figures.
//!
//! Everything is delivered through a [`StreamSink`] or a per-log closure,
//! called on a worker thread. Nothing here outlives the window: `Hidden`
//! ends the sampling, `Gone` ends everything.

use std::collections::HashMap;
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use docklet_engine::{Backoff, Client, ContainerUsage};
use docklet_wsl::usage::{HostUsage, UsageSampler};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::control::{self, EngineHost};
use crate::policy::{HOST_USAGE_INTERVAL, WindowState};

/// An event stream that lasted this long was working, so the next reconnect
/// starts with the shortest wait again.
const STABLE_STREAM: Duration = Duration::from_secs(30);

/// What the event stream tells the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerEvent {
    /// Docker reported a change to something of this kind (`container`,
    /// `image`, `volume`, `network`, ...).
    Changed(String),
    /// The stream ended: the engine stopped or the pipe went away.
    Disconnected,
    /// The stream is about to be opened again. Changes made while nothing
    /// was listening were missed.
    Reconnecting,
}

impl DockerEvent {
    /// The event as one word: the kind, `disconnected` or `reconnecting`.
    pub fn name(&self) -> &str {
        match self {
            Self::Changed(kind) => kind,
            Self::Disconnected => "disconnected",
            Self::Reconnecting => "reconnecting",
        }
    }
}

/// What a followed log sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogMessage {
    /// Lines without their line endings, in batches at most every 50 ms. An
    /// unterminated line is cut at 64 KiB.
    Lines(Vec<String>),
    /// The last message of a log. `error` is `None` when the log simply
    /// ended, which it does when its container stops or is removed.
    Ended { error: Option<String> },
}

/// The host's figures at one moment, or why there are none.
#[derive(Debug, Clone, PartialEq)]
pub enum HostReading {
    Sample(HostUsage),
    /// The figures could not be read; the text says why.
    Failed(String),
}

/// Where the streams deliver. Every method is called on a worker thread and
/// must return quickly: post to the UI thread, do not work here.
pub trait StreamSink: Send + Sync + 'static {
    fn docker_event(&self, event: DockerEvent);

    /// The usage of every running container, about once a second while it
    /// is sampled. `None` when there are no figures any more: the event
    /// stream ended or the engine could not be asked.
    fn container_usage(&self, _usage: Option<Vec<ContainerUsage>>) {}

    /// The host's figures, every two seconds while they are sampled.
    fn host_usage(&self, _reading: HostReading) {}
}

/// The background work that feeds the UI.
pub struct Streams {
    runtime: Handle,
    host: Arc<dyn EngineHost>,
    sink: Arc<dyn StreamSink>,
    state: Mutex<State>,
    next_log: AtomicU32,
    /// Whether container stats are taken now. The event stream's task reads
    /// it: stats are only asked for over a stream that is open.
    container_usage: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    window: WindowState,
    events: Option<JoinHandle<()>>,
    logs: HashMap<u32, JoinHandle<()>>,
    wants_host_usage: bool,
    wants_container_usage: bool,
    /// Dropping it ends the thread that samples the host.
    host_sampler: Option<mpsc::Sender<()>>,
}

impl Streams {
    /// Streams that run on `runtime`, ask `host` whether the engine can be
    /// reached and deliver to `sink`. Nothing runs until the window is said
    /// to exist (`set_window`).
    pub fn new(runtime: Handle, host: Arc<dyn EngineHost>, sink: Arc<dyn StreamSink>) -> Self {
        Self {
            runtime,
            host,
            sink,
            state: Mutex::default(),
            next_log: AtomicU32::new(0),
            container_usage: watch::Sender::new(false),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The state is consistent after every statement that changes it, so
        // it is still good after a panic elsewhere poisoned the lock.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Says what became of the window. `Gone` ends the event subscription,
    /// every followed log and the sampling; `Hidden` ends the sampling,
    /// which starts again with `Visible`.
    pub fn set_window(&self, window: WindowState) {
        let mut state = self.state();
        state.window = window;
        if window == WindowState::Gone {
            self.stop_streams(&mut state);
        }
        self.apply_sampling(&mut state);
    }

    /// Subscribes to Docker's event stream and forwards each event to the
    /// sink, so the UI can reload the affected list. The subscription is
    /// renewed when it ends, until `stop_all`. Idempotent. Does nothing
    /// without a window: nobody would listen, and nothing would stop it when
    /// the window is already gone. The next list view asks again.
    pub fn watch_events(&self) {
        let mut state = self.state();
        if state.window == WindowState::Gone {
            return;
        }
        if state
            .events
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let source = EngineEvents {
            runtime: self.runtime.clone(),
            host: self.host.clone(),
            sink: self.sink.clone(),
            container_usage: self.container_usage.subscribe(),
        };
        let forward = forward_events(source, self.sink.clone());
        state.events = Some(self.runtime.spawn(forward));
    }

    /// A token no log of these streams has had: the name of a log in
    /// `follow_logs` and `stop_logs`.
    pub fn log_token(&self) -> u32 {
        self.next_log.fetch_add(1, Ordering::Relaxed)
    }

    /// Follows a container's logs into `output` under the name `token`,
    /// starting with its last 500 lines. `output` ends the following by
    /// breaking; its last message is always `LogMessage::Ended`, unless the
    /// log is stopped from here.
    pub fn follow_logs(
        &self,
        token: u32,
        client: Client,
        id: String,
        mut output: impl FnMut(LogMessage) -> ControlFlow<()> + Send + 'static,
    ) {
        let mut state = self.state();
        if state.window == WindowState::Gone {
            // Nothing would ever stop it.
            drop(state);
            let _ = output(LogMessage::Ended { error: None });
            return;
        }
        let task = self.runtime.spawn(async move {
            let lines = |lines: Vec<String>| output(LogMessage::Lines(lines));
            let outcome = client.follow_logs(&id, lines).await;
            // Without this a log that ended, or never started, looks like
            // one that is merely quiet.
            let error = outcome.err().map(|err| err.to_string());
            let _ = output(LogMessage::Ended { error });
        });
        // A log that ended on its own, because its container was removed,
        // would otherwise stay here until the UI stops it.
        state.logs.retain(|_, running| !running.is_finished());
        state.logs.insert(token, task);
    }

    pub fn stop_logs(&self, token: u32) {
        if let Some(task) = self.state().logs.remove(&token) {
            task.abort();
        }
    }

    /// Ends the event subscription, with the container stats that ride on
    /// it, and every followed log. The host's figures keep coming: reading
    /// them touches nothing of the engine.
    pub fn stop_all(&self) {
        self.stop_streams(&mut self.state());
    }

    /// Ends the event subscription, with the container stats that ride on
    /// it, and leaves the followed logs alone: for a window that shows a
    /// log and no list, or is minimised with a log open. `watch_events`
    /// subscribes again.
    pub fn stop_events(&self) {
        if let Some(task) = self.state().events.take() {
            task.abort();
        }
    }

    fn stop_streams(&self, state: &mut State) {
        if let Some(task) = state.events.take() {
            task.abort();
        }
        for (_, task) in state.logs.drain() {
            task.abort();
        }
    }

    /// Says which usage figures the UI shows. They are sampled only while
    /// the window is visible: the host's every two seconds, the containers'
    /// about once a second and only over an open event stream
    /// (`watch_events`), which is what proves the engine is up.
    pub fn set_usage(&self, host: bool, containers: bool) {
        let mut state = self.state();
        state.wants_host_usage = host;
        state.wants_container_usage = containers;
        self.apply_sampling(&mut state);
    }

    fn apply_sampling(&self, state: &mut State) {
        let visible = state.window == WindowState::Visible;
        let containers = visible && state.wants_container_usage;
        self.container_usage.send_if_modified(|on| {
            let changed = *on != containers;
            *on = containers;
            changed
        });
        let host = visible && state.wants_host_usage;
        if !host {
            state.host_sampler = None;
        } else if state.host_sampler.is_none() {
            state.host_sampler = Some(sample_host(self.sink.clone()));
        }
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        let mut state = self.state();
        self.stop_streams(&mut state);
        state.host_sampler = None;
    }
}

/// Samples the host on a thread of its own until the returned sender is
/// dropped. The sampler keeps what the CPU and GPU rates need between two
/// samples, and it is not handed between threads.
///
/// Every round ends in a wait of `HOST_USAGE_INTERVAL` or in the thread's
/// end, whether the sample worked or not.
fn sample_host(sink: Arc<dyn StreamSink>) -> mpsc::Sender<()> {
    let (stop, stopped) = mpsc::channel::<()>();
    let failed = sink.clone();
    let sampling = move || {
        let mut sampler = UsageSampler::system();
        loop {
            let sample = match sampler.as_mut() {
                Ok(sampler) => sampler.sample(),
                Err(err) => Err(std::io::Error::new(err.kind(), err.to_string())),
            };
            let unsupported = sample
                .as_ref()
                .is_err_and(|err| err.kind() == std::io::ErrorKind::Unsupported);
            sink.host_usage(match sample {
                Ok(usage) => HostReading::Sample(usage),
                Err(err) => HostReading::Failed(err.to_string()),
            });
            // Asking again cannot change the answer.
            if unsupported {
                break;
            }
            match stopped.recv_timeout(HOST_USAGE_INTERVAL) {
                Err(RecvTimeoutError::Timeout) => {}
                // Stopped: the sender is gone.
                _ => break,
            }
        }
    };
    let thread = std::thread::Builder::new().name("docklet-usage".to_string());
    if let Err(err) = thread.spawn(sampling) {
        failed.host_usage(HostReading::Failed(err.to_string()));
    }
    stop
}

/// Where the event stream comes from. The real one is [`EngineEvents`]; the
/// trait is the seam that lets the reconnect loop be tested without Docker.
trait EventSource: Send + 'static {
    type Connection: Send;

    /// See `control::reachable`. Counts as unreachable if the check fails.
    fn reachable(&self) -> impl Future<Output = bool> + Send;

    /// A connection, on which nothing has been sent yet.
    fn connect(&self) -> Option<Self::Connection>;

    /// Calls `on_event` with the kind of each Docker event until the stream
    /// ends.
    fn watch(
        &self,
        connection: Self::Connection,
        on_event: impl FnMut(String) + Send,
    ) -> impl Future<Output = ()> + Send;
}

/// Forwards Docker's events to the sink and opens the stream again whenever
/// it ends, which it does when the engine is restarted from the CLI, the
/// pipe hiccups or an event cannot be decoded. Runs until it is aborted.
///
/// A sleeping engine is never woken: the stream is only opened when the
/// engine can be reached without starting anything.
///
/// Every round ends in a sleep of at least a second, whether the stream ran,
/// failed at once or was not tried.
async fn forward_events<S: EventSource>(source: S, sink: Arc<dyn StreamSink>) {
    let mut backoff = Backoff::default();
    let mut connected_before = false;
    loop {
        if source.reachable().await
            && let Some(connection) = source.connect()
        {
            if connected_before {
                sink.docker_event(DockerEvent::Reconnecting);
            }
            connected_before = true;
            let opened = Instant::now();
            let forward = |kind: String| sink.docker_event(DockerEvent::Changed(kind));
            source.watch(connection, forward).await;
            sink.docker_event(DockerEvent::Disconnected);
            if opened.elapsed() >= STABLE_STREAM {
                backoff.reset();
            }
        }
        backoff.wait().await;
    }
}

/// The engine's event stream, with the container stats that are taken while
/// it is open.
struct EngineEvents {
    runtime: Handle,
    host: Arc<dyn EngineHost>,
    sink: Arc<dyn StreamSink>,
    container_usage: watch::Receiver<bool>,
}

impl EventSource for EngineEvents {
    type Connection = Client;

    async fn reachable(&self) -> bool {
        let host = self.host.clone();
        let check = move || control::reachable(host.as_ref());
        self.runtime.spawn_blocking(check).await.unwrap_or(false)
    }

    fn connect(&self) -> Option<Client> {
        Client::connect().ok()
    }

    async fn watch(&self, client: Client, on_event: impl FnMut(String) + Send) {
        let sink = self.sink.as_ref();
        let usage = follow_usage(&client, self.container_usage.clone(), sink);
        tokio::select! {
            _ = client.watch_events(on_event) => {}
            () = usage => {}
        }
        if *self.container_usage.borrow() {
            sink.container_usage(None);
        }
    }
}

/// Takes the stats of the running containers for as long as `on` says so.
/// Never returns: it is dropped together with the event stream it runs next
/// to, so no stats are asked of an engine whose stream has ended.
///
/// That matters because every round of stats opens connections. With the
/// engine stopped from outside, a connection through a plain proxy would
/// boot the distro again; the event stream, which ends the moment the engine
/// goes, is what tells in time.
async fn follow_usage(client: &Client, mut on: watch::Receiver<bool>, sink: &dyn StreamSink) {
    let mut backoff = Backoff::default();
    loop {
        if on.wait_for(|on| *on).await.is_err() {
            // The streams are being dropped, and this task with them.
            return std::future::pending().await;
        }
        let delivered = AtomicBool::new(false);
        let deliver = |usage: Vec<ContainerUsage>| {
            delivered.store(true, Ordering::Relaxed);
            sink.container_usage(Some(usage));
            ControlFlow::Continue(())
        };
        let mut off = on.clone();
        let failed = tokio::select! {
            _ = client.follow_usage(deliver) => true,
            _ = off.wait_for(|on| !*on) => false,
        };
        if failed {
            // The engine could not be listed. Told at once, and tried again
            // after a wait of at least a second.
            sink.container_usage(None);
            if delivered.load(Ordering::Relaxed) {
                backoff.reset();
            }
            backoff.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use docklet_core::{EngineState, ProxyMode};

    use super::*;
    use crate::control::testing::FakeHost;

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl StreamSink for Recorder {
        fn docker_event(&self, event: DockerEvent) {
            self.events.lock().unwrap().push(event.name().to_string());
        }
    }

    /// An engine whose stream sends `events` and then stays open for
    /// `lasts`.
    struct FakeSource {
        reachable: Arc<AtomicBool>,
        checks: Arc<AtomicUsize>,
        connections: Arc<AtomicUsize>,
        events: Vec<&'static str>,
        lasts: Duration,
    }

    impl FakeSource {
        fn new(reachable: bool, events: &[&'static str], lasts: Duration) -> Self {
            Self {
                reachable: Arc::new(AtomicBool::new(reachable)),
                checks: Arc::default(),
                connections: Arc::default(),
                events: events.to_vec(),
                lasts,
            }
        }
    }

    impl EventSource for FakeSource {
        type Connection = ();

        async fn reachable(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed);
            self.reachable.load(Ordering::Relaxed)
        }

        fn connect(&self) -> Option<()> {
            self.connections.fetch_add(1, Ordering::Relaxed);
            Some(())
        }

        async fn watch(&self, (): (), mut on_event: impl FnMut(String) + Send) {
            for event in &self.events {
                on_event(event.to_string());
            }
            tokio::time::sleep(self.lasts).await;
        }
    }

    /// Runs the reconnect loop for `time` of the paused clock.
    async fn run_for(source: FakeSource, time: Duration) -> Vec<String> {
        let recorder = Arc::new(Recorder::default());
        let forward = forward_events(source, recorder.clone());
        let _ = tokio::time::timeout(time, forward).await;
        recorder.events()
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_cannot_be_reached_is_never_connected_to() {
        let source = FakeSource::new(false, &["container"], Duration::ZERO);
        let (checks, connections) = (source.checks.clone(), source.connections.clone());
        let events = run_for(source, Duration::from_secs(59)).await;
        assert_eq!(events, [] as [&str; 0]);
        // A sleeping engine would have been woken by a connection.
        assert_eq!(connections.load(Ordering::Relaxed), 0);
        // Asked at 0 s and again after waits of 1, 2, 4, 8, 15 and 15
        // seconds; the next time would be at 60 s. No spinning.
        assert_eq!(checks.load(Ordering::Relaxed), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn events_are_forwarded_and_the_end_of_the_stream_is_told() {
        let source = FakeSource::new(true, &["container", "image"], Duration::from_secs(5));
        let events = run_for(source, Duration::from_millis(5500)).await;
        assert_eq!(events, ["container", "image", "disconnected"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_is_opened_again_says_so_first() {
        let source = FakeSource::new(true, &["container"], Duration::ZERO);
        // Opened at 0 s and again after waits of 1 s and 2 s.
        let events = run_for(source, Duration::from_millis(3500)).await;
        assert_eq!(
            events,
            [
                "container",
                "disconnected",
                "reconnecting",
                "container",
                "disconnected",
                "reconnecting",
                "container",
                "disconnected",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_fails_at_once_is_retried_ever_more_slowly() {
        let source = FakeSource::new(true, &[], Duration::ZERO);
        let connections = source.connections.clone();
        run_for(source, Duration::from_secs(59)).await;
        // At 0, 1, 3, 7, 15, 30 and 45 seconds; the next would be at 60 s.
        assert_eq!(connections.load(Ordering::Relaxed), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_lasted_starts_over_with_the_shortest_wait() {
        let source = FakeSource::new(true, &[], STABLE_STREAM);
        let connections = source.connections.clone();
        run_for(source, Duration::from_secs(100)).await;
        // Each stream lasts 30 s and is followed by a wait of 1 s, not of
        // 1, 2 and 4: opened at 0, 31, 62 and 93 seconds.
        assert_eq!(connections.load(Ordering::Relaxed), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_went_to_sleep_is_left_alone_until_it_is_back() {
        let source = FakeSource::new(true, &[], Duration::from_secs(2));
        let (reachable, connections) = (source.reachable.clone(), source.connections.clone());
        let recorder = Arc::new(Recorder::default());
        let forward = forward_events(source, recorder.clone());
        let asleep = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            reachable.store(false, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(59)).await;
        };
        tokio::select! {
            () = forward => {}
            () = asleep => {}
        }
        assert_eq!(recorder.events(), ["disconnected"]);
        assert_eq!(connections.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn events_have_one_word_names() {
        assert_eq!(DockerEvent::Changed("volume".to_string()).name(), "volume");
        assert_eq!(DockerEvent::Disconnected.name(), "disconnected");
        assert_eq!(DockerEvent::Reconnecting.name(), "reconnecting");
    }

    fn streams(runtime: &tokio::runtime::Runtime) -> (Streams, Arc<Recorder>) {
        // Asleep, so the event task that these tests start connects to
        // nothing.
        let host = FakeHost::new(EngineState::Stopped, ProxyMode::IdleStop);
        let recorder = Arc::new(Recorder::default());
        let streams = Streams::new(runtime.handle().clone(), host, recorder.clone());
        (streams, recorder)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn nothing_is_watched_without_a_window() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        streams.watch_events();
        assert!(streams.state().events.is_none());
        streams.set_window(WindowState::Hidden);
        streams.watch_events();
        assert!(streams.state().events.is_some());
    }

    #[test]
    fn everything_stops_when_the_window_is_gone() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        streams.set_window(WindowState::Visible);
        streams.set_usage(false, true);
        streams.watch_events();
        assert!(*streams.container_usage.borrow());
        streams.set_window(WindowState::Gone);
        assert!(streams.state().events.is_none());
        assert!(!*streams.container_usage.borrow());
        assert!(streams.state().host_sampler.is_none());
    }

    #[test]
    fn container_stats_are_taken_only_while_the_window_is_visible() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        streams.set_usage(false, true);
        assert!(!*streams.container_usage.borrow());
        streams.set_window(WindowState::Visible);
        assert!(*streams.container_usage.borrow());
        streams.set_window(WindowState::Hidden);
        assert!(!*streams.container_usage.borrow());
        // Back without being asked for again.
        streams.set_window(WindowState::Visible);
        assert!(*streams.container_usage.borrow());
        streams.set_usage(false, false);
        assert!(!*streams.container_usage.borrow());
    }

    #[test]
    fn a_stop_ends_the_event_stream_until_it_is_asked_for_again() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        streams.set_window(WindowState::Visible);
        streams.watch_events();
        streams.stop_all();
        assert!(streams.state().events.is_none());
        assert_eq!(streams.state().window, WindowState::Visible);
        streams.watch_events();
        assert!(streams.state().events.is_some());
    }

    #[test]
    fn the_events_can_end_without_the_logs() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        streams.set_window(WindowState::Visible);
        streams.watch_events();
        // A followed log, as far as the bookkeeping goes.
        let log = runtime.spawn(std::future::pending::<()>());
        streams.state().logs.insert(7, log);
        streams.stop_events();
        assert!(streams.state().events.is_none());
        assert!(streams.state().logs.contains_key(&7));
        // Minimised keeps the log too; only `Gone` ends it.
        streams.set_window(WindowState::Hidden);
        assert!(streams.state().logs.contains_key(&7));
        streams.set_window(WindowState::Gone);
        assert!(streams.state().logs.is_empty());
    }

    #[test]
    fn log_tokens_are_not_reused() {
        let runtime = runtime();
        let (streams, _) = streams(&runtime);
        let first = streams.log_token();
        assert_ne!(first, streams.log_token());
    }
}
