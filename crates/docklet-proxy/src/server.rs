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
    /// `wsl.exe -d <distro> -u root --exec socat ...` bridging stdio to the
    /// Docker socket inside the distro.
    pub fn wsl_socat(distro: &str) -> Self {
        let args = [
            "-d",
            distro,
            "-u",
            "root",
            "--exec",
            "socat",
            "-t",
            "86400",
            "STDIO",
            "UNIX-CONNECT:/var/run/docker.sock",
        ];
        Self {
            program: "wsl.exe".into(),
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
    listener: NamedPipeServer,
}

impl PipeProxy {
    /// Creates the pipe. Fails if another process already owns the name.
    pub fn bind(config: PipeProxyConfig) -> io::Result<Self> {
        let security = PipeSecurity::current_user_only()?;
        let listener = create_instance(&config, &security, true)?;
        Ok(Self {
            config: Arc::new(config),
            security,
            listener,
        })
    }

    /// Accepts connections until an accept fails.
    pub async fn run(mut self) -> io::Result<()> {
        let mut next_id = 0u64;
        loop {
            self.listener.connect().await?;
            let next = create_instance(&self.config, &self.security, false)?;
            let pipe = std::mem::replace(&mut self.listener, next);
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
    let mut child_in = child.stdin.take().expect("stdin is piped");
    let mut child_out = child.stdout.take().expect("stdout is piped");
    let (mut pipe_rd, mut pipe_wr) = tokio::io::split(pipe);

    // Client → child. Resolves only when the client is gone; a half-close just
    // closes the child's stdin and keeps watching the pipe.
    let upstream = async move {
        let mut buffer = vec![0u8; RELAY_BUFFER];
        let mut child_in = Some(&mut child_in);
        let mut total = 0u64;
        loop {
            match pipe_rd.read(&mut buffer).await {
                Ok(0) => {
                    if let Some(stdin) = child_in.take() {
                        if trace {
                            eprintln!("[conn {id}] client EOF after {total} bytes");
                        }
                        let _ = stdin.shutdown().await;
                    }
                }
                Ok(n) => {
                    total += n as u64;
                    // Data after EOF is a protocol violation and is dropped.
                    if let Some(stdin) = child_in.as_mut()
                        && stdin.write_all(&buffer[..n]).await.is_err()
                    {
                        return;
                    }
                }
                Err(err) if err.raw_os_error() == Some(ERROR_MORE_DATA) => {
                    if trace {
                        eprintln!("[conn {id}] ERROR_MORE_DATA on read");
                    }
                }
                Err(err) => {
                    if trace {
                        eprintln!("[conn {id}] client read ended: {err}");
                    }
                    return;
                }
            }
        }
    };

    // Child → client.
    let downstream = async {
        let copied = tokio::io::copy(&mut child_out, &mut pipe_wr).await;
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
