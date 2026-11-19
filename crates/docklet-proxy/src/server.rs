//! Named pipe accept loop and per-connection relay.
//!
//! Half-close: the Docker CLI (go-winio) signals "stdin EOF" on a message-mode
//! pipe by writing a zero-byte message, and keeps reading the response. The
//! relay maps that to closing the child's stdin, and keeps the other direction
//! open until the child closes its stdout.
//!
//! With idle stop, each connection first asks the lifecycle controller for
//! admission, naming its client process. A watcher the last idle stop cut
//! off may be told to hold: its connection stays open and unrelayed, what it
//! sends is kept, and it is relayed, with what it sent, once the engine runs.

use std::ffi::OsString;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::Stdio;
use std::sync::Arc;

use docklet_core::idle::{ClientId, FirstRequest, classify_request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use tokio::process::Command;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

use crate::lifecycle::{Admit, Admitted, Client, Cut, IdleStop, Lifecycle, Traffic, describe};
use crate::security::PipeSecurity;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERROR_MORE_DATA: i32 = 234;
const RELAY_BUFFER: usize = 64 * 1024;
/// What `tokio::io::copy` used for the engine's direction, which is kept so
/// the messages clients read are the size they always were.
const DOWNSTREAM_BUFFER: usize = 8 * 1024;
/// A held connection that sends more than this is not only subscribing.
const MAX_HELD_BYTES: usize = 64 * 1024;

/// Program that carries one connection's bytes over its stdin/stdout.
#[derive(Debug, Clone)]
pub struct RelayCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl RelayCommand {
    /// `wsl.exe -d <distro> -u root --exec docklet-relay ...` bridging stdio
    /// to the Docker socket inside the distro. `docklet-relay` ships in the
    /// rootfs (see `distro/relay`).
    pub fn wsl_relay(distro: &str) -> Self {
        let args = [
            "-d",
            distro,
            "-u",
            "root",
            "--exec",
            "/usr/local/bin/docklet-relay",
            "/var/run/docker.sock",
        ];
        Self {
            program: docklet_core::system_program("wsl.exe").into(),
            args: args.iter().map(OsString::from).collect(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct PipeProxyConfig {
    /// Full pipe path, e.g. `\\.\pipe\docklet_engine`.
    pub pipe_name: String,
    /// Message mode is what lets go-winio clients half-close. Byte mode exists
    /// only for comparison in the M0 spike.
    pub message_mode: bool,
    pub relay: RelayCommand,
    /// Log per-connection events to stderr.
    pub trace: bool,
}

pub struct PipeProxy {
    config: Arc<PipeProxyConfig>,
    security: PipeSecurity,
    /// Two unconnected instances. A client can connect to an instance as
    /// soon as it exists, so while one is being handed off and replaced the
    /// other still accepts; with a single instance a client arriving in that
    /// gap gets ERROR_PIPE_BUSY.
    listeners: [NamedPipeServer; 2],
    idle: Option<IdleStop>,
}

impl PipeProxy {
    /// Creates the pipe. Fails if another process already owns the name.
    pub fn bind(config: PipeProxyConfig) -> io::Result<Self> {
        let security = PipeSecurity::current_user_only()?;
        let first = create_instance(&config, &security, true)?;
        let spare = create_instance(&config, &security, false)?;
        Ok(Self {
            config: Arc::new(config),
            security,
            listeners: [first, spare],
            idle: None,
        })
    }

    /// Stops the engine when it is unused and starts it again when a client
    /// connects. Without this the proxy only relays, as it always did.
    pub fn with_idle_stop(mut self, idle: IdleStop) -> Self {
        self.idle = Some(idle);
        self
    }

    /// Accepts connections until an accept fails.
    pub async fn run(mut self) -> io::Result<()> {
        let lifecycle = self
            .idle
            .take()
            .map(|idle| Lifecycle::spawn(idle, self.config.trace));
        let mut next_id = 0u64;
        loop {
            // Wait on both: a client may pick either instance, and one that
            // connected to an instance nobody waits on would hang. `connect`
            // is cancel safe, and on an instance that is already connected it
            // returns at once.
            let [first, spare] = &self.listeners;
            let connected = tokio::select! {
                result = first.connect() => result.map(|()| 0),
                result = spare.connect() => result.map(|()| 1),
            };
            let index = connected?;
            let next = create_instance(&self.config, &self.security, false)?;
            let pipe = std::mem::replace(&mut self.listeners[index], next);
            let config = Arc::clone(&self.config);
            let lifecycle = lifecycle.clone();
            let id = next_id;
            next_id += 1;
            tokio::spawn(async move {
                let outcome = match lifecycle {
                    Some(lifecycle) => serve_with_lifecycle(pipe, &config, id, &lifecycle).await,
                    None => relay(pipe, &config, id, Prelude::default(), None, None).await,
                };
                if config.trace {
                    eprintln!("[conn {id}] closed: {outcome:?}");
                }
            });
        }
    }
}

/// Admits the connection, holding it as long as the controller says so,
/// then relays it. Waiting for the engine happens here, never on the accept
/// loop. The guard counts the connection as in use until this returns,
/// however it returns.
async fn serve_with_lifecycle(
    mut pipe: NamedPipeServer,
    config: &PipeProxyConfig,
    id: u64,
    lifecycle: &Lifecycle,
) -> io::Result<RelayEnd> {
    let client = client_of(&pipe);
    let traffic = lifecycle.traffic();
    let mut prelude = Prelude::default();
    let Admitted { guard, cut } = loop {
        let request = prelude.request();
        let admit = lifecycle
            .admit(id, client.clone(), request, Arc::clone(&traffic))
            .await;
        match admit {
            Admit::Relay(admitted) => break admitted,
            Admit::Refused => return Ok(RelayEnd::NotStarted),
            Admit::Hold(release) => {
                if config.trace {
                    eprintln!(
                        "[conn {id}] held: {} asks for `{}`",
                        describe(client.as_ref()),
                        prelude.line()
                    );
                }
                // Each round waits: for the engine, or for the client. A
                // client that sends nothing more blocks here at no cost.
                tokio::select! {
                    _ = release => {}
                    more = prelude.read_from(&mut pipe) => {
                        if !more {
                            return Ok(RelayEnd::ClientGone);
                        }
                    }
                }
            }
        }
    };
    let outcome = relay(pipe, config, id, prelude, Some(traffic), Some(cut)).await;
    drop(guard);
    outcome
}

/// What a client sent before its connection was relayed: only a held one
/// sends anything then.
#[derive(Debug, Default)]
struct Prelude {
    bytes: Vec<u8>,
    /// The client half-closed: the relay closes the child's stdin at once.
    half_closed: bool,
}

impl Prelude {
    fn request(&self) -> FirstRequest {
        if self.bytes.len() > MAX_HELD_BYTES {
            return FirstRequest::Active;
        }
        classify_request(&self.bytes)
    }

    /// The request line so far, for trace output.
    fn line(&self) -> String {
        let line = self.bytes.split(|byte| *byte == b'\r' || *byte == b'\n');
        String::from_utf8_lossy(line.into_iter().next().unwrap_or_default()).into_owned()
    }

    /// Reads once from a held connection. False when the client is gone.
    ///
    /// The first zero-length read is the client's half-close (after which a
    /// read blocks again); a second one can only mean the client is gone, so
    /// this never completes twice without data on a live connection.
    async fn read_from(&mut self, pipe: &mut NamedPipeServer) -> bool {
        let mut buffer = [0u8; 4096];
        match pipe.read(&mut buffer).await {
            Ok(0) if !self.half_closed => {
                self.half_closed = true;
                true
            }
            Ok(0) | Err(_) => false,
            Ok(n) => {
                // Data after EOF is a protocol violation and is dropped.
                if !self.half_closed {
                    self.bytes.extend_from_slice(&buffer[..n]);
                }
                true
            }
        }
    }
}

/// The process at the other end of the pipe, if Windows says.
fn client_of(pipe: &NamedPipeServer) -> Option<Client> {
    let handle = HANDLE(pipe.as_raw_handle());
    let mut pid = 0u32;
    // SAFETY: `handle` is the pipe's own and stays open while `pipe` is
    // borrowed; `pid` outlives the call.
    unsafe { GetNamedPipeClientProcessId(handle, &mut pid) }.ok()?;
    let (started, program) = process_facts(pid);
    Some(Client {
        id: ClientId { pid, started },
        program,
    })
}

/// When process `pid` started, and the file name of its executable. Zero and
/// `None` for what cannot be read.
fn process_facts(pid: u32) -> (u64, Option<String>) {
    // SAFETY: plain Win32 call; the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return (0, None);
    };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle is open with query rights; the four FILETIMEs
    // outlive the call.
    let times =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    let started = match times {
        Ok(()) => (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        Err(_) => 0,
    };
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` UTF-16 units and outlives the call, which
    // writes at most that many and stores the count it wrote in `len`.
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    let program = named.ok().map(|()| {
        let path = String::from_utf16_lossy(&buffer[..(len as usize).min(buffer.len())]);
        path.rsplit('\\').next().unwrap_or_default().to_string()
    });
    // SAFETY: the handle came from OpenProcess and is not used after this.
    unsafe {
        let _ = CloseHandle(process);
    }
    (started, program)
}

fn create_instance(
    config: &PipeProxyConfig,
    security: &PipeSecurity,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(if config.message_mode {
            PipeMode::Message
        } else {
            PipeMode::Byte
        });
    // SAFETY: the pointer refers to a SECURITY_ATTRIBUTES owned by `security`,
    // which outlives this call.
    unsafe {
        options.create_with_security_attributes_raw(&config.pipe_name, security.as_attributes_ptr())
    }
}

#[derive(Debug)]
enum RelayEnd {
    /// The child closed its stdout: the server side finished the exchange.
    ServerDone {
        #[allow(dead_code, reason = "shown in trace output through Debug")]
        to_client: u64,
    },
    /// The client went away before the server side finished.
    ClientGone,
    /// An idle stop closed the connection: the engine it led to is gone.
    Cut,
    /// The engine did not start, so the connection was never relayed.
    NotStarted,
}

/// Relays one connection through a new child process. `prelude` is what the
/// client sent before; `traffic` counts what moves in either direction;
/// `cut` closes the connection when an idle stop says so.
async fn relay(
    pipe: NamedPipeServer,
    config: &PipeProxyConfig,
    id: u64,
    prelude: Prelude,
    traffic: Option<Arc<Traffic>>,
    cut: Option<Cut>,
) -> io::Result<RelayEnd> {
    let trace = config.trace;
    let mut child = Command::new(&config.relay.program)
        .args(&config.relay.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if trace {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()?;
    let mut child_in = child.stdin.take().expect("stdin is piped");
    let mut child_out = child.stdout.take().expect("stdout is piped");
    let (mut pipe_rd, mut pipe_wr) = tokio::io::split(pipe);
    let count = move |bytes: usize, to_engine: bool| {
        if let Some(traffic) = &traffic {
            if to_engine {
                traffic.to_engine(bytes);
            } else {
                traffic.to_client(bytes);
            }
        }
    };
    let count_up = count.clone();

    // What a held client sent goes first, and a half-close it made then is
    // passed on right after.
    if !prelude.bytes.is_empty() {
        if trace {
            eprintln!("[conn {id}] request: {}", prelude.line());
        }
        if child_in.write_all(&prelude.bytes).await.is_ok() {
            count(prelude.bytes.len(), true);
        }
    }
    let child_in = if prelude.half_closed {
        let _ = child_in.flush().await;
        drop(child_in);
        None
    } else {
        Some(child_in)
    };
    let mut total = prelude.bytes.len() as u64;

    // Client → child. Resolves only when the client is gone.
    //
    // A zero-length read is ambiguous: it is either the client's half-close
    // (a zero-byte message, after which reads block again) or a disconnect
    // (after which every read returns zero immediately). The first one closes
    // the child's stdin; a second one can only mean the client is gone. The
    // loop must never retry a read that keeps completing without data, or it
    // would spin a CPU core per connection.
    let upstream = async move {
        let mut buffer = vec![0u8; RELAY_BUFFER];
        let mut child_in = child_in;
        loop {
            match pipe_rd.read(&mut buffer).await {
                Ok(0) => match child_in.take() {
                    Some(mut stdin) => {
                        if trace {
                            eprintln!("[conn {id}] client EOF after {total} bytes");
                        }
                        // Only dropping the handle closes the child's stdin;
                        // `shutdown` on a process pipe just flushes.
                        let _ = stdin.flush().await;
                        drop(stdin);
                    }
                    None => {
                        if trace {
                            eprintln!("[conn {id}] second zero-length read: client gone");
                        }
                        return;
                    }
                },
                Ok(n) => {
                    if trace && total == 0 {
                        let line = buffer[..n].split(|b| *b == b'\r').next().unwrap_or(&[]);
                        eprintln!("[conn {id}] request: {}", String::from_utf8_lossy(line));
                    }
                    total += n as u64;
                    count_up(n, true);
                    // Data after EOF is a protocol violation and is dropped.
                    if let Some(stdin) = child_in.as_mut()
                        && stdin.write_all(&buffer[..n]).await.is_err()
                    {
                        return;
                    }
                }
                Err(err) => {
                    if trace {
                        let more = err.raw_os_error() == Some(ERROR_MORE_DATA);
                        eprintln!("[conn {id}] client read ended: {err} (more_data={more})");
                    }
                    return;
                }
            }
            // Let the other direction and other connections run even if the
            // pipe keeps this task ready.
            tokio::task::yield_now().await;
        }
    };
    // Child → client, as `tokio::io::copy` did it, counting as it goes.
    let downstream = async {
        let mut buffer = vec![0u8; DOWNSTREAM_BUFFER];
        let mut copied = 0u64;
        let outcome = loop {
            match child_out.read(&mut buffer).await {
                Ok(0) => break Ok(copied),
                Ok(n) => {
                    if let Err(err) = pipe_wr.write_all(&buffer[..n]).await {
                        break Err(err);
                    }
                    copied += n as u64;
                    count(n, false);
                }
                Err(err) => break Err(err),
            }
        };
        if trace {
            eprintln!("[conn {id}] relay stdout ended: {outcome:?}");
        }
        let _ = pipe_wr.flush().await;
        outcome
    };
    let cut = async {
        match cut {
            Some(cut) => cut.wait().await,
            None => std::future::pending().await,
        }
    };

    let end = tokio::select! {
        copied = downstream => RelayEnd::ServerDone { to_client: copied? },
        () = upstream => RelayEnd::ClientGone,
        // Dropping the pipe below closes it; the client reads its end.
        () = cut => RelayEnd::Cut,
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::windows::named_pipe::ClientOptions;

    const ERROR_PIPE_BUSY: i32 = 231;

    /// The accept loop runs on the test's own task, so both clients of a pair
    /// connect before it gets to replace any instance: the second one needs
    /// the spare, and with a single instance it got "pipe busy" every time.
    /// Both replies arriving means the loop has replaced both instances
    /// before the next pair. A burst larger than the number of free instances
    /// can still race the loop, so the test does not try one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runner only"]
    async fn concurrent_clients_never_get_pipe_busy() {
        let pipe_name = format!(r"\\.\pipe\docklet_test_busy_{}", std::process::id());
        let config = PipeProxyConfig {
            pipe_name: pipe_name.clone(),
            message_mode: true,
            // Answers every connection with one line and exits.
            relay: RelayCommand {
                program: docklet_core::system_program("cmd.exe").into(),
                args: ["/d", "/c", "echo", "docklet"]
                    .iter()
                    .map(OsString::from)
                    .collect(),
            },
            trace: false,
        };
        let proxy = PipeProxy::bind(config).unwrap();

        let clients = async {
            for round in 0..4 {
                let mut pair = Vec::new();
                for slot in 0..2 {
                    match ClientOptions::new().open(&pipe_name) {
                        Ok(client) => pair.push(client),
                        Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                            panic!("round {round}, client {slot}: pipe busy")
                        }
                        Err(err) => panic!("round {round}, client {slot}: {err}"),
                    }
                }
                for (slot, mut client) in pair.into_iter().enumerate() {
                    let mut reply = Vec::new();
                    client.read_to_end(&mut reply).await.unwrap();
                    assert_eq!(
                        String::from_utf8_lossy(&reply).trim(),
                        "docklet",
                        "round {round}, client {slot}"
                    );
                }
            }
        };
        let both = async {
            tokio::select! {
                served = proxy.run() => panic!("the proxy stopped: {served:?}"),
                () = clients => {}
            }
        };
        tokio::time::timeout(Duration::from_secs(60), both)
            .await
            .expect("clients did not finish within 60 s");
    }

    /// A real proxy with fake engine hooks: the first client starts the
    /// engine, two more that connect during the start share it and are served
    /// after it, and once nobody is connected the engine is stopped exactly
    /// once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runner only"]
    async fn the_first_client_wakes_the_engine_and_an_idle_engine_stops_once() {
        use crate::lifecycle::tests::{FakeEngine, eventually};
        use docklet_core::idle::IdlePolicy;
        use std::sync::atomic::Ordering;
        use std::time::Instant;

        let pipe_name = format!(r"\\.\pipe\docklet_test_idle_{}", std::process::id());
        let config = PipeProxyConfig {
            pipe_name: pipe_name.clone(),
            message_mode: true,
            relay: RelayCommand {
                program: docklet_core::system_program("cmd.exe").into(),
                args: ["/d", "/c", "echo", "docklet"]
                    .iter()
                    .map(OsString::from)
                    .collect(),
            },
            trace: true,
        };
        let engine = Arc::new(FakeEngine {
            start_delay: Duration::from_secs(2),
            ..FakeEngine::default()
        });
        let policy =
            IdlePolicy::new(Duration::from_secs(2)).with_check_every(Duration::from_secs(1));
        let proxy = PipeProxy::bind(config)
            .unwrap()
            .with_idle_stop(IdleStop::new(policy, Arc::clone(&engine)));

        let reply_of = |mut client: tokio::net::windows::named_pipe::NamedPipeClient| async move {
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).await.unwrap();
            (
                String::from_utf8_lossy(&reply).trim().to_string(),
                Instant::now(),
            )
        };
        let scenario = async {
            let first = ClientOptions::new().open(&pipe_name).unwrap();
            assert!(
                eventually(Duration::from_secs(10), || engine
                    .starts
                    .load(Ordering::SeqCst)
                    == 1)
                .await,
                "the first client did not start the engine"
            );
            // The start takes two seconds; these two arrive while it runs.
            let second = ClientOptions::new().open(&pipe_name).unwrap();
            let third = ClientOptions::new().open(&pipe_name).unwrap();
            let replies = tokio::join!(reply_of(first), reply_of(second), reply_of(third));
            let (began, ended) = engine.start_log.lock().unwrap()[0];
            for (slot, (reply, at)) in [replies.0, replies.1, replies.2].into_iter().enumerate() {
                assert_eq!(reply, "docklet", "client {slot}");
                assert!(
                    at >= ended,
                    "client {slot} was served before the start ended"
                );
            }
            assert!(ended - began >= Duration::from_secs(2));
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1, "one shared start");
            assert_eq!(engine.stops.load(Ordering::SeqCst), 0);

            assert!(
                eventually(Duration::from_secs(15), || engine
                    .stops
                    .load(Ordering::SeqCst)
                    >= 1)
                .await,
                "the idle engine was not stopped"
            );
            // Stopped stays stopped and quiet until the next client.
            let checks = engine.checks.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert_eq!(
                engine.stops.load(Ordering::SeqCst),
                1,
                "stopped exactly once"
            );
            assert_eq!(
                engine.checks.load(Ordering::SeqCst),
                checks,
                "checks after the stop"
            );
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
        };
        let both = async {
            tokio::select! {
                served = proxy.run() => panic!("the proxy stopped: {served:?}"),
                () = scenario => {}
            }
        };
        tokio::time::timeout(Duration::from_secs(60), both)
            .await
            .expect("the scenario did not finish within 60 s");
    }

    /// Reads one line, for at most `limit`.
    async fn first_line(
        client: &mut tokio::net::windows::named_pipe::NamedPipeClient,
        limit: Duration,
    ) -> String {
        let mut reply = Vec::new();
        let mut buffer = [0u8; 256];
        let read = async {
            while !reply.contains(&b'\n') {
                match client.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => reply.extend_from_slice(&buffer[..n]),
                }
            }
        };
        tokio::time::timeout(limit, read)
            .await
            .expect("no line within the limit");
        String::from_utf8_lossy(&reply).trim().to_string()
    }

    /// Docker Desktop's case with real pipes: a client that stays connected
    /// and quiet does not keep the engine awake, is closed by the stop, and
    /// its reconnect only subscribes, so it waits without waking the engine.
    /// A real request wakes it, and the held connection is relayed then.
    /// All clients are this test process, which is what makes the second
    /// connection a returning watcher.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runner only"]
    async fn a_quiet_watcher_does_not_keep_the_engine_awake_nor_wake_it_again() {
        use crate::lifecycle::tests::{FakeEngine, eventually};
        use docklet_core::idle::IdlePolicy;
        use std::sync::atomic::Ordering;

        let pipe_name = format!(r"\\.\pipe\docklet_test_watcher_{}", std::process::id());
        let config = PipeProxyConfig {
            pipe_name: pipe_name.clone(),
            message_mode: true,
            // Answers with one line, then stays up until its stdin closes,
            // like a relay to `docker events`.
            relay: RelayCommand {
                program: docklet_core::system_program("cmd.exe").into(),
                args: ["/d", "/c", "echo", "docklet&", "more"]
                    .iter()
                    .map(OsString::from)
                    .collect(),
            },
            trace: true,
        };
        let engine = Arc::new(FakeEngine::default());
        let policy =
            IdlePolicy::new(Duration::from_secs(2)).with_check_every(Duration::from_secs(1));
        let proxy = PipeProxy::bind(config)
            .unwrap()
            .with_idle_stop(IdleStop::new(policy, Arc::clone(&engine)));
        let limit = Duration::from_secs(15);

        let scenario = async {
            let mut watcher = ClientOptions::new().open(&pipe_name).unwrap();
            watcher
                .write_all(b"GET /events HTTP/1.1\r\nHost: docker\r\n\r\n")
                .await
                .unwrap();
            assert_eq!(first_line(&mut watcher, limit).await, "docklet");
            assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
            // Quiet from here on, and still the engine stops.
            assert!(
                eventually(limit, || engine.stops.load(Ordering::SeqCst) == 1).await,
                "a quiet connection kept the engine awake"
            );
            // The stop closed the connection.
            let mut rest = Vec::new();
            tokio::time::timeout(limit, watcher.read_to_end(&mut rest))
                .await
                .expect("the quiet connection was not closed")
                .ok();
            // It comes back and subscribes again: held, no start.
            let mut back = ClientOptions::new().open(&pipe_name).unwrap();
            back.write_all(b"GET /v1.47/events HTTP/1.1\r\nHost: docker\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert_eq!(
                engine.starts.load(Ordering::SeqCst),
                1,
                "the returning watcher woke the engine"
            );
            // A real request wakes it, and both are relayed.
            let mut user = ClientOptions::new().open(&pipe_name).unwrap();
            user.write_all(b"GET /v1.47/containers/json HTTP/1.1\r\nHost: docker\r\n\r\n")
                .await
                .unwrap();
            assert_eq!(first_line(&mut user, limit).await, "docklet");
            assert_eq!(engine.starts.load(Ordering::SeqCst), 2);
            assert_eq!(first_line(&mut back, limit).await, "docklet");
        };
        let both = async {
            tokio::select! {
                served = proxy.run() => panic!("the proxy stopped: {served:?}"),
                () = scenario => {}
            }
        };
        tokio::time::timeout(Duration::from_secs(90), both)
            .await
            .expect("the scenario did not finish within 90 s");
    }
}
