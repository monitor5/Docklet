//! `docklet-relay [SOCKET]`: bridges stdin/stdout to a unix socket.
//!
//! Runs inside the docklet distro, started once per Docker API connection by
//! the Windows pipe proxy through `wsl.exe --exec`. It exists because neither
//! socat nor nc has the asymmetric end-of-stream behaviour the Docker API
//! needs:
//!
//! - stdin EOF (the client half-closed, e.g. `docker run -i` after its input
//!   ends) shuts down only the write side of the socket and keeps relaying
//!   the response for as long as the daemon sends it;
//! - socket EOF (the daemon finished, which for attach streams is itself only
//!   a half-close) ends the process immediately, because exiting is the only
//!   end-of-stream signal that `wsl.exe` passes on to Windows.

use std::fs::File;
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::net::Shutdown;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::process::exit;

const DEFAULT_SOCKET: &str = "/var/run/docker.sock";
const BUFFER: usize = 64 * 1024;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCKET.to_string());
    let socket = UnixStream::connect(&path).unwrap_or_else(|err| {
        eprintln!("docklet-relay: {path}: {err}");
        exit(1)
    });
    let mut upstream = socket.try_clone().unwrap_or_else(|err| {
        eprintln!("docklet-relay: {err}");
        exit(1)
    });

    // stdin → socket. Ends with a half-close; the main thread decides when
    // the process exits.
    std::thread::spawn(move || {
        // SAFETY: fd 0 is open for the life of the process and never closed
        // here (ManuallyDrop). Raw fds avoid std's buffering and locking.
        let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
        let mut buffer = vec![0u8; BUFFER];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if upstream.write_all(&buffer[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = upstream.shutdown(Shutdown::Write);
    });

    // socket → stdout.
    let mut downstream = socket;
    // SAFETY: as above, for fd 1.
    let mut stdout = ManuallyDrop::new(unsafe { File::from_raw_fd(1) });
    let mut buffer = vec![0u8; BUFFER];
    loop {
        match downstream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
        }
    }
    exit(0)
}
