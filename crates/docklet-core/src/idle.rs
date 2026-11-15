//! When to stop an idle engine and when to start it again, as pure logic.
//!
//! The pipe proxy sees every Docker connection, so it decides. It feeds
//! events into an `IdleMachine` (a connection opened or closed, an engine call
//! finished, a deadline passed) and carries out the `IdleAction`s it gets
//! back. The machine never blocks and never reads a clock: every call takes
//! `now`, so the tests drive it with a fake clock.
//!
//! The engine is stopped when, for `IdlePolicy::idle_after`, no connection was
//! open and no check saw a running container. Checks run only while no
//! connection is open and the distro is not known to be stopped, at most once
//! per `IdlePolicy::check_every`. While the distro is stopped nothing is
//! scheduled at all; the next connection starts it.

use std::io;
use std::time::{Duration, Instant};

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

/// How long the engine may sit unused before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdlePolicy {
    idle_after: Duration,
    check_every: Duration,
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

    /// Stops the engine after `idle_after` without connections or running
    /// containers. Checks run every `idle_after`, but at least once a minute
    /// and at most once a second.
    pub fn new(idle_after: Duration) -> Self {
        let idle_after = idle_after.min(Self::MAX);
        Self {
            idle_after,
            check_every: idle_after.clamp(Self::MIN_CHECK_EVERY, Self::DEFAULT_CHECK_EVERY),
        }
    }

    /// Sets the time between two checks, at least `MIN_CHECK_EVERY`.
    pub fn with_check_every(mut self, every: Duration) -> Self {
        self.check_every = every.clamp(Self::MIN_CHECK_EVERY, Self::MAX);
        self
    }

    pub fn idle_after(&self) -> Duration {
        self.idle_after
    }

    pub fn check_every(&self) -> Duration {
        self.check_every
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

/// A blocking engine call the driver must run, then report back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    /// Call `EngineHooks::start_engine`, then `start_finished`.
    Start,
    /// Call `EngineHooks::stop_distro`, then `stop_finished`.
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
}

/// Idle stop and wake on connect, without any I/O. See the module docs.
#[derive(Debug)]
pub struct IdleMachine {
    policy: IdlePolicy,
    phase: EnginePhase,
    connections: usize,
    /// Since when the engine has been unused. Meaningful only while it can
    /// be idle (see `idle_clock_runs`).
    idle_since: Instant,
    next_check: Instant,
    last_check: Option<Instant>,
    checking: bool,
    /// A connection came and went while the check ran, so its result may
    /// no longer hold.
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
            connections: 0,
            idle_since: now,
            next_check: now,
            last_check: None,
            checking: false,
            check_stale: false,
        };
        machine.restart_idle_clock(now);
        machine
    }

    pub fn phase(&self) -> EnginePhase {
        self.phase
    }

    pub fn connections(&self) -> usize {
        self.connections
    }

    /// When the driver must call `on_deadline`, or `None` to wait for the
    /// next event without a timer.
    pub fn deadline(&self) -> Option<Instant> {
        (self.idle_clock_runs() && !self.checking).then_some(self.next_check)
    }

    /// The deadline passed. Returns `Check` when one is due.
    pub fn on_deadline(&mut self, now: Instant) -> Option<IdleAction> {
        if !self.deadline().is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.checking = true;
        self.check_stale = false;
        self.last_check = Some(now);
        Some(IdleAction::Check)
    }

    /// A client connected. The connection counts as in use until the matching
    /// `disconnect`, whatever `Admission` says.
    pub fn connect(&mut self, _now: Instant) -> (Admission, Option<IdleAction>) {
        self.connections += 1;
        if self.checking {
            self.check_stale = true;
        }
        match self.phase {
            EnginePhase::Running => (Admission::Proceed, None),
            EnginePhase::Starting | EnginePhase::Stopping => (Admission::Wait, None),
            EnginePhase::Unknown | EnginePhase::Stopped => {
                self.phase = EnginePhase::Starting;
                (Admission::Wait, Some(IdleAction::Start))
            }
        }
    }

    /// A connection ended, on any path.
    pub fn disconnect(&mut self, now: Instant) {
        self.connections = self.connections.saturating_sub(1);
        if self.idle_clock_runs() {
            self.restart_idle_clock(now);
        }
    }

    /// The start ended. Returns whether the waiting connections may proceed;
    /// if not, the driver closes them and the next connection starts again.
    pub fn start_finished(&mut self, now: Instant, started: bool) -> bool {
        self.phase = if started {
            EnginePhase::Running
        } else {
            EnginePhase::Unknown
        };
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
            EnginePhase::Unknown
        };
        if self.connections > 0 {
            self.phase = EnginePhase::Starting;
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
        // A connection came, or a start began, while the check ran: what it
        // saw is out of date. The clock was restarted when the last
        // connection closed.
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

    /// The engine may be up and nothing uses it, so time counts towards a
    /// stop.
    fn idle_clock_runs(&self) -> bool {
        self.connections == 0 && matches!(self.phase, EnginePhase::Running | EnginePhase::Unknown)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SECOND: Duration = Duration::from_secs(1);

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
            let start = Instant::now();
            let mut sim = Self {
                start,
                machine: IdleMachine::new(policy(), start),
            };
            // One client brings the engine up and leaves at t = 0.
            assert_eq!(sim.connect(0), (Admission::Wait, Some(IdleAction::Start)));
            assert!(sim.machine.start_finished(sim.at(0), true));
            sim.disconnect(0);
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

        fn connect(&mut self, secs: u64) -> (Admission, Option<IdleAction>) {
            let now = self.at(secs);
            self.machine.connect(now)
        }

        fn disconnect(&mut self, secs: u64) {
            let now = self.at(secs);
            self.machine.disconnect(now);
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
        machine.disconnect(Instant::now());
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
    }

    #[test]
    fn open_connections_pause_the_clock_and_closing_restarts_it() {
        let mut sim = Sim::new();
        assert_eq!(sim.check(60, Activity::Containers(0)), None);
        assert_eq!(sim.connect(100), (Admission::Proceed, None));
        assert_eq!(sim.connect(110), (Admission::Proceed, None));
        // A long-lived connection (`docker events`) keeps the engine in use
        // and no check runs meanwhile.
        assert_eq!(sim.machine.deadline(), None);
        assert_eq!(sim.machine.on_deadline(sim.at(10_000)), None);
        sim.disconnect(1000);
        assert_eq!(sim.machine.deadline(), None, "one connection is still open");
        sim.disconnect(2000);
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
        assert_eq!(sim.connect(500), (Admission::Wait, Some(IdleAction::Start)));
    }

    #[test]
    fn concurrent_connections_share_one_start() {
        let start = Instant::now();
        let mut machine = IdleMachine::new(policy(), start);
        assert_eq!(machine.phase(), EnginePhase::Unknown);
        assert_eq!(
            machine.connect(start),
            (Admission::Wait, Some(IdleAction::Start))
        );
        for _ in 0..2 {
            assert_eq!(machine.connect(start + SECOND), (Admission::Wait, None));
        }
        assert_eq!(machine.deadline(), None, "no check while starting");
        assert!(machine.start_finished(start + 20 * SECOND, true));
        assert_eq!(machine.phase(), EnginePhase::Running);
        assert_eq!(
            machine.connect(start + 21 * SECOND),
            (Admission::Proceed, None)
        );
        assert_eq!(machine.connections(), 4);
    }

    #[test]
    fn a_failed_start_releases_the_waiters_and_the_next_connection_retries() {
        let start = Instant::now();
        let mut machine = IdleMachine::new(policy(), start);
        assert_eq!(machine.connect(start).1, Some(IdleAction::Start));
        assert_eq!(machine.connect(start).1, None);
        assert!(!machine.start_finished(start + SECOND, false));
        assert_eq!(machine.phase(), EnginePhase::Unknown);
        // The driver closes both waiting connections.
        machine.disconnect(start + SECOND);
        machine.disconnect(start + SECOND);
        // The distro may be half up; checks find out and stop it if unused.
        assert!(machine.deadline().is_some());
        assert_eq!(
            machine.connect(start + 2 * SECOND),
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
        assert_eq!(sim.connect(301), (Admission::Wait, None));
        assert_eq!(sim.connect(302), (Admission::Wait, None));
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
        for secs in [60, 120, 180, 240] {
            assert_eq!(sim.check(secs, Activity::Containers(0)), None);
        }
        assert_eq!(
            sim.check(300, Activity::Containers(0)),
            Some(IdleAction::Stop)
        );
        assert_eq!(sim.machine.stop_finished(sim.at(310), false), None);
        assert_eq!(sim.machine.phase(), EnginePhase::Unknown);
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
        assert_eq!(sim.connect(301), (Admission::Proceed, None));
        sim.disconnect(302);
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
        assert_eq!(sim.connect(61), (Admission::Proceed, None));
        sim.disconnect(61);
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
