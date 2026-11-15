//! Splitting an image reference for the Docker API.

use crate::Error;

const DEFAULT_TAG: &str = "latest";

/// An image reference split into the `fromImage` and `tag` parameters of the
/// Docker API's image pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRef<'a> {
    /// Repository, with its registry host and port if the reference has one.
    pub name: &'a str,
    /// Tag, or the digest (`sha256:...`) when the reference pins one.
    pub tag: &'a str,
}

impl<'a> ImageRef<'a> {
    /// Parses what a user would give to `docker pull`.
    ///
    /// The tag defaults to `latest`. An empty tag is rejected rather than
    /// passed on, because the Docker API reads it as "pull every tag".
    pub fn parse(reference: &'a str) -> Result<Self, Error> {
        let trimmed = reference.trim();
        let (name, tag) = match trimmed.split_once('@') {
            Some((name, digest)) => {
                // The digest pins the image, so a tag next to it is dropped,
                // as `docker pull` does.
                let name = split_tag(name).map_or(name, |(name, _)| name);
                (name, digest)
            }
            None => split_tag(trimmed).unwrap_or((trimmed, DEFAULT_TAG)),
        };
        if name.is_empty() || tag.is_empty() || trimmed.contains(char::is_whitespace) {
            return Err(Error::InvalidImageRef(reference.to_string()));
        }
        Ok(Self { name, tag })
    }
}

/// Splits `name:tag`. A colon separates a tag only in the last path segment;
/// one before a slash belongs to a registry port.
fn split_tag(name: &str) -> Option<(&str, &str)> {
    let (name, tag) = name.rsplit_once(':')?;
    (!tag.contains('/')).then_some((name, tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(reference: &str) -> (&str, &str) {
        let image = ImageRef::parse(reference).unwrap();
        (image.name, image.tag)
    }

    fn is_invalid(reference: &str) -> bool {
        matches!(ImageRef::parse(reference), Err(Error::InvalidImageRef(_)))
    }

    #[test]
    fn tag_defaults_to_latest() {
        assert_eq!(split("nginx"), ("nginx", "latest"));
        assert_eq!(split("library/nginx"), ("library/nginx", "latest"));
        assert_eq!(split("ghcr.io/owner/app"), ("ghcr.io/owner/app", "latest"));
    }

    #[test]
    fn explicit_tag() {
        assert_eq!(split("nginx:1.29-alpine"), ("nginx", "1.29-alpine"));
        assert_eq!(split("ghcr.io/owner/app:v2"), ("ghcr.io/owner/app", "v2"));
    }

    #[test]
    fn registry_port_is_not_a_tag() {
        assert_eq!(
            split("localhost:5000/nginx"),
            ("localhost:5000/nginx", "latest")
        );
        assert_eq!(
            split("localhost:5000/team/nginx:1.29"),
            ("localhost:5000/team/nginx", "1.29")
        );
        assert_eq!(split("[::1]:5000/nginx"), ("[::1]:5000/nginx", "latest"));
    }

    #[test]
    fn digest_goes_into_the_tag_parameter() {
        assert_eq!(split("nginx@sha256:0a1b"), ("nginx", "sha256:0a1b"));
        assert_eq!(
            split("localhost:5000/nginx@sha256:0a1b"),
            ("localhost:5000/nginx", "sha256:0a1b")
        );
    }

    #[test]
    fn digest_wins_over_a_tag() {
        assert_eq!(split("nginx:1.29@sha256:0a1b"), ("nginx", "sha256:0a1b"));
        assert_eq!(
            split("localhost:5000/nginx:1.29@sha256:0a1b"),
            ("localhost:5000/nginx", "sha256:0a1b")
        );
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(split("  nginx:1.29\n"), ("nginx", "1.29"));
    }

    #[test]
    fn incomplete_references_are_rejected() {
        for reference in ["", "   ", "nginx:", ":1.29", "nginx@", "@sha256:0a1b"] {
            assert!(is_invalid(reference), "{reference:?} was accepted");
        }
        assert!(is_invalid("ng inx"));
    }

    #[test]
    fn the_error_quotes_the_reference() {
        let err = ImageRef::parse("nginx:").unwrap_err();
        assert_eq!(err.to_string(), "invalid image reference `nginx:`");
    }
}
