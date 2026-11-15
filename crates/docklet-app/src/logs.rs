//! The lines of a followed log, for a shell that paints only the lines in
//! view.

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::format::format_count;
use crate::streams::LogMessage;

/// Lines kept in memory per log view; older lines are dropped.
pub const LOG_BUFFER_CAP: usize = 50_000;

/// A bounded buffer of log lines. Lives on the UI thread; the batches
/// arrive as `LogMessage`s.
///
/// To paint: ask for `len`, then for `display_line` of each index in view.
/// Paint again when `generation` differs from the one last painted.
/// `first_index + index` numbers a line for good, so a view that is not at
/// the bottom can stay on the same lines while older ones are dropped.
#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    cap: usize,
    /// Number of lines dropped from the front so far.
    dropped: u64,
    generation: u64,
    ended: bool,
    error: Option<String>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_cap(LOG_BUFFER_CAP)
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer that keeps the newest `cap` lines.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            cap,
            dropped: 0,
            generation: 0,
            ended: false,
            error: None,
        }
    }

    /// The number of lines kept.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Absolute number of the first line kept: how many were dropped.
    pub fn first_index(&self) -> u64 {
        self.dropped
    }

    /// Changes with every append and clear, and when the log ends.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The line at `index` as the container wrote it, colour codes
    /// included: what to copy to the clipboard.
    pub fn line(&self, index: usize) -> Option<&str> {
        self.lines.get(index).map(String::as_str)
    }

    /// The line at `index` without ANSI sequences: what to paint.
    pub fn display_line(&self, index: usize) -> Option<Cow<'_, str>> {
        self.line(index).map(strip_ansi)
    }

    /// Appends lines, without a line ending they may still carry, and drops
    /// the oldest ones beyond the cap.
    pub fn append(&mut self, lines: impl IntoIterator<Item = String>) {
        for mut line in lines {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            self.lines.push_back(line);
        }
        let excess = self.lines.len().saturating_sub(self.cap);
        self.lines.drain(..excess);
        self.dropped += excess as u64;
        self.generation += 1;
    }

    /// Takes in what a followed log sent.
    pub fn apply(&mut self, message: LogMessage) {
        match message {
            LogMessage::Lines(lines) => self.append(lines),
            LogMessage::Ended { error } => {
                self.ended = true;
                self.error = error;
                self.generation += 1;
            }
        }
    }

    /// Empties the buffer for another log.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
        self.ended = false;
        self.error = None;
        self.generation += 1;
    }

    /// True when the log is over. Without this a finished log looks like a
    /// quiet one.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Why the log ended, if it did not simply end with its container.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The line under the log's title: `12,345 lines · log ended`.
    pub fn status(&self) -> String {
        let count = self.len();
        let lines = if count == 1 { "line" } else { "lines" };
        let ending = match (self.ended, &self.error) {
            (false, _) => "",
            (true, None) => " · log ended",
            (true, Some(_)) => " · log stopped",
        };
        format!("{} {lines}{ending}", format_count(count))
    }
}

/// Removes CSI sequences such as colours (`ESC [ 32 m`): the escape
/// character, `[`, parameter bytes, intermediate bytes and a final byte.
pub fn strip_ansi(line: &str) -> Cow<'_, str> {
    const ESCAPE: u8 = 0x1b;
    if !line.as_bytes().contains(&ESCAPE) {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let mut text = String::with_capacity(line.len());
    // Sequences are ASCII, so the text between them is cut at character
    // boundaries.
    let (mut kept, mut at) = (0, 0);
    while at < bytes.len() {
        match (bytes[at] == ESCAPE).then(|| csi_end(bytes, at)).flatten() {
            Some(end) => {
                text.push_str(&line[kept..at]);
                kept = end;
                at = end;
            }
            None => at += 1,
        }
    }
    text.push_str(&line[kept..]);
    Cow::Owned(text)
}

/// Where the CSI sequence that starts with the escape character at `start`
/// ends, if one starts there.
fn csi_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start + 1) != Some(&b'[') {
        return None;
    }
    let skip = |from: usize, wanted: fn(u8) -> bool| {
        let run = bytes[from..].iter().take_while(|&&byte| wanted(byte));
        from + run.count()
    };
    let parameters = skip(start + 2, |byte| {
        byte.is_ascii_digit() || matches!(byte, b';' | b'?')
    });
    let intermediates = skip(parameters, |byte| (b' '..=b'/').contains(&byte));
    let last = *bytes.get(intermediates)?;
    (b'@'..=b'~').contains(&last).then_some(intermediates + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(buffer: &LogBuffer) -> Vec<&str> {
        (0..buffer.len())
            .map(|index| buffer.line(index).unwrap())
            .collect()
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn appends_in_order_and_strips_line_endings() {
        let mut buffer = LogBuffer::with_cap(10);
        buffer.append(lines(&["a\n", "b\r\n"]));
        buffer.append(lines(&["c"]));
        assert_eq!(contents(&buffer), ["a", "b", "c"]);
    }

    #[test]
    fn only_one_line_ending_is_stripped() {
        let mut buffer = LogBuffer::with_cap(10);
        buffer.append(lines(&["a\n\n", "b\r", "\n"]));
        assert_eq!(contents(&buffer), ["a\n", "b\r", ""]);
    }

    #[test]
    fn drops_the_oldest_lines_beyond_the_cap() {
        let mut buffer = LogBuffer::with_cap(3);
        buffer.append(lines(&["1", "2", "3", "4", "5"]));
        assert_eq!(contents(&buffer), ["3", "4", "5"]);
        assert_eq!(buffer.first_index(), 2);
    }

    #[test]
    fn stays_consistent_over_many_appends() {
        let mut buffer = LogBuffer::with_cap(4);
        for index in 0..100 {
            buffer.append([format!("line {index}")]);
        }
        assert_eq!(
            contents(&buffer),
            ["line 96", "line 97", "line 98", "line 99"]
        );
        assert_eq!(buffer.first_index(), 96);
    }

    #[test]
    fn handles_a_single_batch_larger_than_the_cap() {
        let mut buffer = LogBuffer::with_cap(2);
        buffer.append(lines(&["a", "b", "c", "d"]));
        assert_eq!(contents(&buffer), ["c", "d"]);
    }

    #[test]
    fn clears() {
        let mut buffer = LogBuffer::with_cap(2);
        buffer.append(lines(&["a", "b", "c"]));
        buffer.apply(LogMessage::Ended { error: None });
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.first_index(), 0);
        assert_eq!(buffer.line(0), None);
        assert!(!buffer.ended());
    }

    #[test]
    fn keeps_fifty_thousand_lines_by_default() {
        let mut buffer = LogBuffer::new();
        buffer.append((0..LOG_BUFFER_CAP + 10).map(|index| index.to_string()));
        assert_eq!(buffer.len(), 50_000);
        assert_eq!(buffer.first_index(), 10);
        assert_eq!(buffer.line(0), Some("10"));
    }

    #[test]
    fn the_generation_changes_with_everything_a_view_must_repaint_for() {
        let mut buffer = LogBuffer::with_cap(2);
        let start = buffer.generation();
        buffer.apply(LogMessage::Lines(lines(&["a"])));
        assert_eq!(buffer.generation(), start + 1);
        buffer.apply(LogMessage::Ended { error: None });
        assert_eq!(buffer.generation(), start + 2);
        buffer.clear();
        assert_eq!(buffer.generation(), start + 3);
    }

    #[test]
    fn says_how_a_log_ended() {
        let mut buffer = LogBuffer::with_cap(5000);
        assert_eq!(buffer.status(), "0 lines");
        buffer.append(lines(&["a"]));
        assert_eq!(buffer.status(), "1 line");
        buffer.append((0..1233).map(|index| index.to_string()));
        assert_eq!(buffer.status(), "1,234 lines");
        buffer.apply(LogMessage::Ended { error: None });
        assert!(buffer.ended());
        assert_eq!(buffer.error(), None);
        assert_eq!(buffer.status(), "1,234 lines · log ended");

        let mut failed = LogBuffer::new();
        failed.apply(LogMessage::Ended {
            error: Some("no such container".to_string()),
        });
        assert_eq!(failed.error(), Some("no such container"));
        assert_eq!(failed.status(), "0 lines · log stopped");
    }

    #[test]
    fn removes_colour_codes() {
        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m done"), "ok done");
    }

    #[test]
    fn leaves_plain_text_alone() {
        let line = "plain [1] text";
        assert!(matches!(strip_ansi(line), Cow::Borrowed(text) if text == line));
    }

    #[test]
    fn removes_sequences_with_parameters_and_intermediates() {
        assert_eq!(strip_ansi("\u{1b}[1;38;5;208mwarn\u{1b}[m"), "warn");
        assert_eq!(strip_ansi("a\u{1b}[?25lb\u{1b}[2Kc"), "abc");
        assert_eq!(strip_ansi("a\u{1b}[0 qb"), "ab");
    }

    #[test]
    fn keeps_what_is_not_a_complete_sequence() {
        // An escape character without `[`, and a sequence cut off by the
        // end of the line.
        assert_eq!(strip_ansi("a\u{1b}b"), "a\u{1b}b");
        assert_eq!(strip_ansi("tail\u{1b}[3"), "tail\u{1b}[3");
        assert_eq!(strip_ansi("\u{1b}"), "\u{1b}");
    }

    #[test]
    fn keeps_text_that_is_not_ascii() {
        assert_eq!(strip_ansi("\u{1b}[31m오류\u{1b}[0m ✓"), "오류 ✓");
    }

    #[test]
    fn the_painted_line_has_no_codes_and_the_kept_one_has() {
        let mut buffer = LogBuffer::with_cap(2);
        buffer.append(lines(&["\u{1b}[32mok\u{1b}[0m"]));
        assert_eq!(buffer.display_line(0).as_deref(), Some("ok"));
        assert_eq!(buffer.line(0), Some("\u{1b}[32mok\u{1b}[0m"));
        assert_eq!(buffer.display_line(1), None);
    }
}
