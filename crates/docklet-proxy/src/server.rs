//! Named pipe accept loop and per-connection relay.
//!
//! Half-close: the Docker CLI (go-winio) signals "stdin EOF" on a message-mode
//! pipe by writing a zero-byte message, and keeps reading the response. The
//! relay maps that to closing the child's stdin, and keeps the other direction
//! open until the child closes its stdout.

use std::ffi::OsString;
use std::io;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use tokio::process::Command;

use crate::security::PipeSecurity;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERROR_MORE_DATA: i32 = 234;
const RELAY_BUFFER: usize = 64 * 1024;

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
        })
    }

    /// Accepts connections until an accept fails.
    pub async fn run(mut self) -> io::Result<()> {
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
            let id = next_id;
            next_id += 1;
            tokio::spawn(async move {
                let outcome = relay(pipe, &config, id).await;
                if config.trace {
                    eprintln!("[conn {id}] closed: {outcome:?}");
                }
            });
        }
    }
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
}

async fn relay(pipe: NamedPipeServer, config: &PipeProxyConfig, id: u64) -> io::Result<RelayEnd> {
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
    let child_in = child.stdin.take().expect("stdin is piped");
    let mut child_out = child.stdout.take().expect("stdout is piped");
    let (mut pipe_rd, mut pipe_wr) = tokio::io::split(pipe);

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
        let mut child_in = Some(child_in);
        let mut total = 0u64;
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
    // Child → client.
    let downstream = async {
        let copied = tokio::io::copy(&mut child_out, &mut pipe_wr).await;
        if trace {
            eprintln!("[conn {id}] relay stdout ended: {copied:?}");
        }
        let _ = pipe_wr.flush().await;
        copied
    };

    let end = tokio::select! {
        copied = downstream => RelayEnd::ServerDone { to_client: copied? },
        () = upstream => RelayEnd::ClientGone,
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
}
