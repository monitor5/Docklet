//! Progress lines for an image pull.

/// Formats one pull update as `<layer> <status> <percent>%`, leaving out the
/// parts Docker did not send. `progress` is bytes done and bytes in total.
pub fn progress_line(
    id: Option<&str>,
    status: Option<&str>,
    progress: Option<(i64, i64)>,
) -> String {
    let done = progress
        .and_then(|(current, total)| percent(current, total))
        .map(|percent| format!("{percent}%"));
    [id, status, done.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whole percent of a layer that is done. `None` while Docker does not know
/// the total, which it reports as zero or less.
fn percent(current: i64, total: i64) -> Option<i64> {
    (total > 0).then(|| (current.saturating_mul(100) / total).clamp(0, 100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_status_and_percent() {
        let line = progress_line(Some("a1b2c3"), Some("Downloading"), Some((50, 200)));
        assert_eq!(line, "a1b2c3 Downloading 25%");
    }

    #[test]
    fn missing_parts_are_left_out() {
        assert_eq!(
            progress_line(None, Some("Pulling from library/nginx"), None),
            "Pulling from library/nginx"
        );
        assert_eq!(
            progress_line(Some("a1b2c3"), Some("Pull complete"), None),
            "a1b2c3 Pull complete"
        );
        assert_eq!(progress_line(None, None, None), "");
    }

    #[test]
    fn no_percent_without_a_total() {
        assert_eq!(
            progress_line(Some("a1b2c3"), Some("Extracting"), Some((512, 0))),
            "a1b2c3 Extracting"
        );
        assert_eq!(percent(512, -1), None);
    }

    #[test]
    fn percent_rounds_down_and_stays_in_range() {
        assert_eq!(percent(0, 3), Some(0));
        assert_eq!(percent(2, 3), Some(66));
        assert_eq!(percent(3, 3), Some(100));
        assert_eq!(percent(4, 3), Some(100));
        assert_eq!(percent(-1, 3), Some(0));
    }
}
