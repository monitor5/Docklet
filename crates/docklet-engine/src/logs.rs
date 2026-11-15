//! Turning a container's log stream into batches of lines.

use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;

use futures_util::stream::{Stream, StreamExt};
use tokio::time::MissedTickBehavior;

/// Longest unterminated line that is buffered. Output that never sends a
/// newline (a progress bar redrawn with `\r`, binary data) is cut here so it
/// cannot grow without bound.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Reassembles lines from chunks that may end anywhere, even inside a
/// multi-byte character.
#[derive(Debug, Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    /// Feeds one chunk and appends every line it completes to `lines`,
    /// without the line ending.
    pub fn push(&mut self, chunk: &[u8], lines: &mut Vec<String>) {
        let mut rest = chunk;
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            self.partial.extend_from_slice(&rest[..end]);
            lines.push(self.take_line());
            rest = &rest[end + 1..];
        }
        self.partial.extend_from_slice(rest);
        if self.partial.len() >= MAX_LINE_BYTES {
            lines.push(self.take_line());
        }
    }

    /// Returns the last line if the stream ended without a newline.
    pub fn finish(&mut self) -> Option<String> {
        (!self.partial.is_empty()).then(|| self.take_line())
    }

    fn take_line(&mut self) -> String {
        let line = String::from_utf8_lossy(&self.partial)
            .trim_end_matches('\r')
            .to_string();
        self.partial.clear();
        line
    }
}

/// Reads `chunks` to its end and hands the lines to `sink` in batches, at
/// most one batch per `period`, so a chatty container cannot flood the
/// receiver. Stops early when `sink` breaks.
///
/// A stream error ends the reading too; the lines read before it are still
/// delivered, then the error is returned.
pub(crate) async fn batch_lines<S, B, E>(
    chunks: S,
    period: Duration,
    mut sink: impl FnMut(Vec<String>) -> ControlFlow<()>,
) -> Result<(), E>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    let mut chunks = pin!(chunks);
    let mut splitter = LineSplitter::default();
    let mut batch = Vec::new();
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            chunk = chunks.next() => match chunk {
                Some(Ok(bytes)) => splitter.push(bytes.as_ref(), &mut batch),
                Some(Err(err)) => break Err(err),
                None => break Ok(()),
            },
            _ = tick.tick() => {
                if !batch.is_empty() && sink(std::mem::take(&mut batch)).is_break() {
                    return Ok(());
                }
            }
        }
    };
    batch.extend(splitter.finish());
    if !batch.is_empty() {
        let _ = sink(batch);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use futures_util::stream;

    use super::*;

    const PERIOD: Duration = Duration::from_millis(50);

    fn split(chunks: &[&str]) -> Vec<String> {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        for chunk in chunks {
            splitter.push(chunk.as_bytes(), &mut lines);
        }
        lines.extend(splitter.finish());
        lines
    }

    #[test]
    fn whole_lines_in_one_chunk() {
        assert_eq!(split(&["one\ntwo\n"]), ["one", "two"]);
    }

    #[test]
    fn a_line_spanning_chunks() {
        assert_eq!(split(&["on", "e\ntw", "o", "\n"]), ["one", "two"]);
    }

    #[test]
    fn crlf_line_endings() {
        assert_eq!(split(&["one\r\ntwo\r\n"]), ["one", "two"]);
        // The carriage return and the newline arrive in different chunks.
        assert_eq!(split(&["one\r", "\ntwo\r\n"]), ["one", "two"]);
    }

    #[test]
    fn empty_lines_are_kept() {
        assert_eq!(split(&["one\n\ntwo\n"]), ["one", "", "two"]);
    }

    #[test]
    fn final_line_without_a_newline() {
        assert_eq!(split(&["one\ntw", "o"]), ["one", "two"]);
    }

    #[test]
    fn nothing_left_after_a_trailing_newline() {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        splitter.push(b"one\n", &mut lines);
        assert_eq!(lines, ["one"]);
        assert_eq!(splitter.finish(), None);
        assert!(split(&[]).is_empty());
    }

    #[test]
    fn a_character_split_between_chunks() {
        let bytes = "h\u{e9}llo\n".as_bytes();
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        // Byte 2 is the second half of the two-byte `é`.
        splitter.push(&bytes[..2], &mut lines);
        splitter.push(&bytes[2..], &mut lines);
        assert_eq!(lines, ["h\u{e9}llo"]);
    }

    #[test]
    fn an_endless_line_is_cut() {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        let endless = "x".repeat(MAX_LINE_BYTES);
        splitter.push(endless.as_bytes(), &mut lines);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        splitter.push(b"tail\n", &mut lines);
        assert_eq!(lines[1], "tail");
    }

    /// Runs `batch_lines` over an in-memory stream and returns its outcome
    /// with every line the sink was given.
    async fn collect<E>(
        chunks: impl Stream<Item = Result<&'static str, E>>,
    ) -> (Result<(), E>, Vec<String>) {
        let mut seen = Vec::new();
        let outcome = batch_lines(chunks, PERIOD, |batch| {
            seen.extend(batch);
            ControlFlow::Continue(())
        })
        .await;
        (outcome, seen)
    }

    #[tokio::test(start_paused = true)]
    async fn every_line_reaches_the_sink_in_order() {
        let chunks = stream::iter([Ok::<_, ()>("one\ntw"), Ok("o\r\nthr"), Ok("ee")]);
        let (outcome, seen) = collect(chunks).await;
        assert_eq!(outcome, Ok(()));
        assert_eq!(seen, ["one", "two", "three"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_error_comes_after_the_lines_before_it() {
        let chunks = stream::iter([Ok("one\ntwo"), Err("boom"), Ok("never\n")]);
        let (outcome, seen) = collect(chunks).await;
        assert_eq!(outcome, Err("boom"));
        assert_eq!(seen, ["one", "two"]);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_when_the_sink_breaks() {
        // Like a followed log, this stream never ends: only the sink can.
        let chunks = stream::iter([Ok::<_, ()>("one\n")]).chain(stream::pending());
        let mut batches = 0;
        let following = batch_lines(chunks, PERIOD, |_| {
            batches += 1;
            ControlFlow::Break(())
        });
        // Paused time: the limit costs nothing and fails a run that would
        // otherwise never return.
        let outcome = tokio::time::timeout(Duration::from_secs(60), following).await;
        assert_eq!(outcome.expect("still following"), Ok(()));
        assert_eq!(batches, 1);
    }
}
