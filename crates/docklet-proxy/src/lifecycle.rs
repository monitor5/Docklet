//! Idle stop and wake on connect: drives `docklet_core::idle::IdleMachine`.
//!
//! One controller task owns the machine, so every decision is taken in one
//! place and in the order events arrived. Connections talk to it over a
//! channel; engine calls run on the blocking pool and report back over the
//! same channel. The controller waits on that channel, and on a timer only
//! when the machine asks for one, so an engine that is stopped or in use
//! costs nothing.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use docklet_core::idle::{
    Activity, Admission, EngineHooks, IdleAction, IdleMachine, IdlePolicy, check_activity,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, WeakUnboundedSender};
use tokio::sync::oneshot;

/// Turns on idle stop and wake on connect for a `PipeProxy`.
#[derive(Clone)]
pub struct IdleStop {
    policy: IdlePolicy,
    hooks: Arc<dyn EngineHooks>,
}

impl IdleStop {
    /// Stops the engine through `hooks` after it was unused for the policy's
    /// idle time, and starts it again when a client connects.
    pub fn new(policy: IdlePolicy, hooks: impl EngineHooks + 'static) -> Self {
        Self {
            policy,
            hooks: Arc::new(hooks),
        }
    }
}

impl std::fmt::Debug for IdleStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleStop")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

enum Event {
    /// A connection arrived. The reply says whether it may be relayed.
    Connect(oneshot::Sender<bool>),
    Disconnect,
    Started(bool),
    Stopped(bool),
    Checked(Activity),
}

/// Handle to the controller task.
#[derive(Clone)]
pub(crate) struct Lifecycle {
    events: UnboundedSender<Event>,
}

impl Lifecycle {
    /// Starts the controller on the current runtime. It ends when every
    /// handle and every connection guard is gone.
    pub(crate) fn spawn(idle: IdleStop, trace: bool) -> Self {
        let (events, inbox) = mpsc::unbounded_channel();
        tokio::spawn(control(idle, trace, inbox, events.downgrade()));
        Self { events }
    }

    /// Counts a new connection in and waits until the engine can serve it.
    /// `None` means the engine did not start and the connection must be
    /// closed. The connection counts as in use until the returned guard, or
    /// on failure this future, is dropped.
    pub(crate) async fn admit(&self) -> Option<ConnectionGuard> {
        let (reply, ready) = oneshot::channel();
        self.events.send(Event::Connect(reply)).ok()?;
        // Created right after the controller heard of the connection, with no
        // await in between, so every path out of here counts it out again.
        let guard = ConnectionGuard {
            events: self.events.clone(),
        };
        matches!(ready.await, Ok(true)).then_some(guard)
    }
}

/// Counts its connection as open until dropped, on every exit path of the
/// relay including errors and panics.
pub(crate) struct ConnectionGuard {
    events: UnboundedSender<Event>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Disconnect);
    }
}

async fn control(
    idle: IdleStop,
    trace: bool,
    mut inbox: UnboundedReceiver<Event>,
    outbox: WeakUnboundedSender<Event>,
) {
    let IdleStop { policy, hooks } = idle;
    let mut machine = IdleMachine::new(policy, Instant::now());
    let mut waiting: Vec<oneshot::Sender<bool>> = Vec::new();
    loop {
        let event = match machine.deadline() {
            Some(deadline) => {
                tokio::select! {
                    event = inbox.recv() => event,
                    () = tokio::time::sleep_until(deadline.into()) => {
                        // `max` guarantees the deadline counts as passed, so
                        // the machine moves on and this never spins.
                        let now = Instant::now().max(deadline);
                        if let Some(action) = machine.on_deadline(now) {
                            perform(action, &hooks, &outbox, trace);
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
        let now = Instant::now();
        let action = match event {
            Event::Connect(reply) => {
                let (admission, action) = machine.connect(now);
                match admission {
                    Admission::Proceed => {
                        let _ = reply.send(true);
                    }
                    Admission::Wait => waiting.push(reply),
                }
                action
            }
            Event::Disconnect => {
                machine.disconnect(now);
                None
            }
            Event::Started(started) => {
                let proceed = machine.start_finished(now, started);
                for reply in waiting.drain(..) {
                    let _ = reply.send(proceed);
                }
                None
            }
            Event::Stopped(stopped) => machine.stop_finished(now, stopped),
            Event::Checked(activity) => machine.check_finished(now, activity),
        };
        if let Some(action) = action {
            perform(action, &hooks, &outbox, trace);
        }
    }
}

/// Runs an engine call on the blocking pool and reports its outcome to the
/// controller. A call that panics counts as failed, so no connection waits
/// for a start that will never report back.
fn perform(
    action: IdleAction,
    hooks: &Arc<dyn EngineHooks>,
    outbox: &WeakUnboundedSender<Event>,
    trace: bool,
) {
    let Some(outbox) = outbox.upgrade() else {
        return;
    };
    let hooks = Arc::clone(hooks);
    if trace {
        eprintln!("[engine] {action:?}");
    }
    tokio::task::spawn_blocking(move || {
        let event = match action {
            IdleAction::Start => Event::Started(succeeded("start", trace, || hooks.start_engine())),
            IdleAction::Stop => Event::Stopped(succeeded("stop", trace, || hooks.stop_distro())),
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
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

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

    /// No pipe and no child process: the controller with fake hooks only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_share_one_start_and_the_idle_engine_stops_once() {
        let engine = Arc::new(FakeEngine {
            start_delay: Duration::from_millis(300),
            ..FakeEngine::default()
        });
        let lifecycle = Lifecycle::spawn(idle_stop(&engine), false);
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
            assert!(lifecycle.admit().await.is_some());
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
        let lifecycle = Lifecycle::spawn(idle_stop(&engine), false);
        let scenario = async {
            let admitted = admit_three(&lifecycle).await;
            assert!(admitted.iter().all(Option::is_none));
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
            engine.broken.store(false, Ordering::SeqCst);
            assert!(lifecycle.admit().await.is_some());
            assert_eq!(engine.starts.load(Ordering::SeqCst), 2);
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_relay_still_counts_its_connection_out() {
        let engine = Arc::new(FakeEngine::default());
        let lifecycle = Lifecycle::spawn(idle_stop(&engine), false);
        let scenario = async {
            let task = tokio::spawn({
                let lifecycle = lifecycle.clone();
                async move {
                    let _guard = lifecycle.admit().await.expect("admitted");
                    panic!("relay failed");
                }
            });
            assert!(task.await.unwrap_err().is_panic());
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    == 1)
                .await,
                "the panicked connection kept the engine in use"
            );
        };
        tokio::time::timeout(Duration::from_secs(30), scenario)
            .await
            .expect("the scenario did not finish within 30 s");
    }

    /// Three connections arriving together.
    async fn admit_three(lifecycle: &Lifecycle) -> [Option<ConnectionGuard>; 3] {
        let (a, b, c) = tokio::join!(lifecycle.admit(), lifecycle.admit(), lifecycle.admit());
        [a, b, c]
    }
}
