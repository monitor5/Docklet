//! Pulling an image: checking what was typed, and the progress lines.

use std::collections::VecDeque;

use docklet_engine::ImageRef;

/// Progress lines kept while an image is pulled; older ones are dropped.
pub const PULL_LINES_CAP: usize = 200;

/// Why what was typed cannot be pulled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    /// Nothing was typed: the Pull button stays disabled, nothing is said.
    Empty,
    /// Not an image reference; the text says so, quoting it.
    Invalid(String),
}

/// Checks an image reference as typed (`nginx`, `nginx:1.29`,
/// `ghcr.io/owner/app@sha256:...`) and returns it without the whitespace
/// around it, ready for `docklet_engine::Client::pull_image`.
///
/// An empty tag is turned down rather than passed on, because the Docker
/// API reads it as "pull every tag".
pub fn check_reference(typed: &str) -> Result<String, ReferenceError> {
    let reference = typed.trim();
    if reference.is_empty() {
        return Err(ReferenceError::Empty);
    }
    match ImageRef::parse(reference) {
        Ok(_) => Ok(reference.to_string()),
        Err(error) => Err(ReferenceError::Invalid(error.to_string())),
    }
}

/// Whether the Pull button is enabled: something was typed and no pull is
/// in flight.
pub fn can_pull(typed: &str, pulling: bool) -> bool {
    !pulling && !typed.trim().is_empty()
}

/// The label of the Pull button.
pub fn pull_button_label(pulling: bool) -> &'static str {
    if pulling { "Pulling…" } else { "Pull" }
}

/// The newest progress lines of a pull, as `<layer> <status> <percent>%`
/// (see `docklet_engine::progress_line`). Lives on the UI thread.
#[derive(Debug)]
pub struct PullProgress {
    lines: VecDeque<String>,
    cap: usize,
    generation: u64,
}

impl Default for PullProgress {
    fn default() -> Self {
        Self::with_cap(PULL_LINES_CAP)
    }
}

impl PullProgress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            cap,
            generation: 0,
        }
    }

    /// Appends a progress line, keeping only the newest ones.
    pub fn push(&mut self, line: String) {
        self.lines.push_back(line);
        let excess = self.lines.len().saturating_sub(self.cap);
        self.lines.drain(..excess);
        self.generation += 1;
    }

    /// Empties the lines when another pull starts.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.generation += 1;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn line(&self, index: usize) -> Option<&str> {
        self.lines.get(index).map(String::as_str)
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Changes with every line and clear.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushed(cap: usize, lines: &[&str]) -> Vec<String> {
        let mut progress = PullProgress::with_cap(cap);
        for line in lines {
            progress.push(line.to_string());
        }
        progress.lines().map(str::to_string).collect()
    }

    #[test]
    fn keeps_the_newest_lines() {
        assert_eq!(pushed(2, &["a", "b", "c"]), ["b", "c"]);
        assert_eq!(pushed(2, &["a"]), ["a"]);
    }

    #[test]
    fn keeps_two_hundred_lines_by_default() {
        let mut progress = PullProgress::new();
        for index in 0..PULL_LINES_CAP + 5 {
            progress.push(index.to_string());
        }
        assert_eq!(progress.len(), 200);
        assert_eq!(progress.line(0), Some("5"));
        assert_eq!(progress.line(199), Some("204"));
        assert_eq!(progress.line(200), None);
    }

    #[test]
    fn clears_for_the_next_pull() {
        let mut progress = PullProgress::new();
        progress.push("a".to_string());
        let before = progress.generation();
        progress.clear();
        assert!(progress.is_empty());
        assert!(progress.generation() > before);
    }

    #[test]
    fn a_reference_is_trimmed() {
        assert_eq!(check_reference("  nginx:1.29\n").unwrap(), "nginx:1.29");
        assert_eq!(check_reference("nginx").unwrap(), "nginx");
        assert_eq!(
            check_reference("localhost:5000/team/nginx@sha256:0a1b").unwrap(),
            "localhost:5000/team/nginx@sha256:0a1b"
        );
    }

    #[test]
    fn nothing_typed_is_not_an_error_to_show() {
        assert_eq!(check_reference(""), Err(ReferenceError::Empty));
        assert_eq!(check_reference("   "), Err(ReferenceError::Empty));
    }

    #[test]
    fn what_is_not_a_reference_is_turned_down_with_its_text() {
        for typed in ["nginx:", ":1.29", "nginx@", "ng inx"] {
            let error = check_reference(typed).unwrap_err();
            let expected = format!("invalid image reference `{typed}`");
            assert_eq!(error, ReferenceError::Invalid(expected));
        }
    }

    #[test]
    fn the_button_needs_text_and_no_pull_in_flight() {
        assert!(can_pull("nginx", false));
        assert!(!can_pull("nginx", true));
        assert!(!can_pull("  ", false));
        assert_eq!(pull_button_label(false), "Pull");
        assert_eq!(pull_button_label(true), "Pulling…");
    }
}
