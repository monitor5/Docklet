//! Pulling an image: checking what was typed, and what its button says.

use docklet_engine::ImageRef;

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

#[cfg(test)]
mod tests {
    use super::*;

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
