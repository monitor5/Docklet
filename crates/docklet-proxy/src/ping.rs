//! One Docker API request through the pipe, the way any Docker client makes
//! it: `GET /_ping`.
//!
//! This is how Docklet itself asks an idle-stop proxy for the engine. The
//! proxy starts a sleeping engine before it relays the request, and counts
//! the connection as use, so the engine's idle time starts over when the
//! request is done. Starting the distro behind the proxy's back would leave
//! it up with the proxy still believing it stopped, and nothing would ever
//! stop it again.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

/// How long to wait for the answer. The proxy may have to start the engine
/// first, which `docklet-wsl` gives two minutes.
pub const PING_TIMEOUT: Duration = Duration::from_secs(150);

const REQUEST: &[u8] = b"GET /_ping HTTP/1.1\r\nHost: docklet\r\nConnection: close\r\n\r\n";
/// Far more than a status line needs; bounds what is read of an answer that
/// is not HTTP.
const HEAD_LIMIT: usize = 4096;
const ERROR_PIPE_BUSY: i32 = 231;

/// Asks Docker for a ping through the engine pipe and waits for its answer,
/// for at most `timeout`. Succeeds when Docker answered with HTTP 200.
///
/// Blocks. The pipe only exists on Windows; elsewhere this fails.
pub fn ping(timeout: Duration) -> io::Result<()> {
    ping_pipe(docklet_core::engine::ENGINE_PIPE, timeout)
}

fn ping_pipe(pipe_name: &str, timeout: Duration) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    let pipe_name = pipe_name.to_string();
    // The pipe is read with blocking calls that have no timeout of their
    // own, so the exchange runs on a thread and the wait for it is bounded
    // here. A thread left behind ends when the proxy answers or closes the
    // connection, which it does once its own bounded engine start is over.
    std::thread::spawn(move || {
        let _ = sender.send(exchange(&pipe_name));
    });
    receiver.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "Docker did not answer through the pipe within {} s",
                timeout.as_secs()
            ),
        ))
    })
}

fn exchange(pipe_name: &str) -> io::Result<()> {
    let mut pipe = open(pipe_name)?;
    pipe.write_all(REQUEST)?;
    let mut head = Vec::new();
    let mut buffer = [0u8; 512];
    // Ends with the first line, the end of the stream or the limit. A read
    // that returns nothing ends it too, so this cannot spin.
    while !head.contains(&b'\n') && head.len() < HEAD_LIMIT {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => head.extend_from_slice(&buffer[..n]),
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => break,
            Err(err) => return Err(err),
        }
    }
    judge(&head)
}

/// Opens the pipe as a client. Every instance of the pipe can be taken for
/// a moment while the proxy replaces one; that is retried for two seconds,
/// with a pause each time.
fn open(pipe_name: &str) -> io::Result<File> {
    let mut busy = 0;
    loop {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && busy < 40 => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            opened => return opened,
        }
    }
}

/// Whether the beginning of an answer is Docker saying yes.
fn judge(head: &[u8]) -> io::Result<()> {
    match status_code(head) {
        Some(200) => Ok(()),
        Some(code) => Err(io::Error::other(format!(
            "Docker answered the ping with HTTP {code}"
        ))),
        // What a proxy with idle stop does when the engine did not start.
        None if head.is_empty() => Err(io::Error::other(
            "the pipe proxy closed the connection without an answer",
        )),
        None => Err(io::Error::other("the answer to the ping was not HTTP")),
    }
}

/// The status code of an HTTP response, from its first line.
fn status_code(head: &[u8]) -> Option<u16> {
    let line = head.split(|byte| *byte == b'\n').next()?;
    let mut words = std::str::from_utf8(line).ok()?.split_whitespace();
    if !words.next()?.starts_with("HTTP/") {
        return None;
    }
    words.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_code_is_read_from_the_first_line() {
        assert_eq!(status_code(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(status_code(b"HTTP/1.0 200 OK\n"), Some(200));
        assert_eq!(
            status_code(b"HTTP/1.1 200 OK\r\nApi-Version: 1.54\r\n\r\nOK"),
            Some(200)
        );
        assert_eq!(
            status_code(b"HTTP/1.1 503 Service Unavailable\r\n"),
            Some(503)
        );
        // The line may still be on its way; what is there is enough.
        assert_eq!(status_code(b"HTTP/1.1 200"), Some(200));
    }

    #[test]
    fn anything_else_has_no_status_code() {
        for head in [
            &b""[..],
            b"\r\n",
            b"HTTP/1.1",
            b"HTTP/1.1 OK\r\n",
            b"docklet\r\n",
            b"200 OK\r\n",
            b"\xff\xfe 200\r\n",
            b"docklet\r\nHTTP/1.1 200 OK\r\n",
        ] {
            assert_eq!(status_code(head), None, "{head:?}");
        }
    }

    #[test]
    fn only_a_200_is_a_yes() {
        assert!(judge(b"HTTP/1.1 200 OK\r\n\r\nOK").is_ok());
        let refused = judge(b"HTTP/1.1 500 Internal Server Error\r\n").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Docker answered the ping with HTTP 500"
        );
        let closed = judge(b"").unwrap_err();
        assert!(closed.to_string().contains("closed the connection"));
        let noise = judge(b"docklet\r\n").unwrap_err();
        assert!(noise.to_string().contains("not HTTP"));
    }

    #[test]
    fn the_request_is_one_complete_http_request() {
        let text = std::str::from_utf8(REQUEST).unwrap();
        assert!(text.starts_with("GET /_ping HTTP/1.1\r\n"));
        assert!(text.contains("\r\nHost: "));
        assert!(text.ends_with("\r\n\r\n"));
    }

    /// A real proxy whose relay answers every connection with one line.
    #[cfg(windows)]
    async fn ping_a_proxy_that_answers(tag: &str, line: &str) -> io::Result<()> {
        use crate::server::{PipeProxy, PipeProxyConfig, RelayCommand};
        use std::ffi::OsString;

        let pipe_name = format!(r"\\.\pipe\docklet_test_ping_{tag}_{}", std::process::id());
        let mut args: Vec<OsString> = ["/d", "/c", "echo"].iter().map(OsString::from).collect();
        args.extend(line.split(' ').map(OsString::from));
        let config = PipeProxyConfig {
            pipe_name: pipe_name.clone(),
            message_mode: true,
            relay: RelayCommand {
                program: docklet_core::system_program("cmd.exe").into(),
                args,
            },
            trace: false,
        };
        let proxy = PipeProxy::bind(config).unwrap();
        let client =
            tokio::task::spawn_blocking(move || ping_pipe(&pipe_name, Duration::from_secs(30)));
        let both = async {
            tokio::select! {
                served = proxy.run() => panic!("the proxy stopped: {served:?}"),
                outcome = client => outcome.expect("the ping panicked"),
            }
        };
        tokio::time::timeout(Duration::from_secs(60), both)
            .await
            .expect("the ping did not finish within 60 s")
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runner only"]
    async fn a_ping_through_a_real_pipe_reads_the_answer() {
        ping_a_proxy_that_answers("ok", "HTTP/1.1 200 OK")
            .await
            .unwrap();
        let refused = ping_a_proxy_that_answers("down", "HTTP/1.1 503 Service Unavailable")
            .await
            .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Docker answered the ping with HTTP 503"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "runner only"]
    fn a_ping_without_a_proxy_fails_at_once() {
        let pipe_name = format!(r"\\.\pipe\docklet_test_ping_none_{}", std::process::id());
        let started = std::time::Instant::now();
        let err = ping_pipe(&pipe_name, Duration::from_secs(30)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
