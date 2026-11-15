//! Display text for Docker's values.

use std::collections::BTreeSet;

use docklet_engine::models::{
    ContainerSummary, ContainerSummaryStateEnum, ImageSummary, PortSummary, PortSummaryTypeEnum,
};

const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// What a cell or a meter shows for a figure that is not known (yet).
pub const UNKNOWN: &str = "—";

/// A size in decimal units, as Docker prints sizes: `412 MB`, `2.3 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 || unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A percentage with one decimal, as `docker stats` prints it: `12.5%`.
pub fn format_percent(percent: f64) -> String {
    format!("{percent:.1}%")
}

/// A count with thousands separators: `12,345`.
pub fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// The UTC date of a Unix timestamp as `2026-11-15`.
pub fn format_date(unix_seconds: i64) -> String {
    let (year, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Year, month and day of a day count since 1970-01-01, in the proleptic
/// Gregorian calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The first `count` characters of `text`.
fn head(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

/// `/web` → `web`; falls back to the short ID.
pub fn container_name(container: &ContainerSummary) -> String {
    let name = container
        .names
        .as_ref()
        .and_then(|names| names.first())
        .map(|name| name.strip_prefix('/').unwrap_or(name))
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => name.to_string(),
        None => head(container.id.as_deref().unwrap_or_default(), 12),
    }
}

/// The container's state as Docker names it (`running`, `exited`, ...), or
/// `unknown` when the engine did not say.
pub fn container_state(container: &ContainerSummary) -> &str {
    match container.state.as_ref().map(AsRef::as_ref) {
        Some(state) if !state.is_empty() => state,
        _ => "unknown",
    }
}

pub fn is_running(container: &ContainerSummary) -> bool {
    container.state == Some(ContainerSummaryStateEnum::RUNNING)
}

/// Host ports published on TCP, without duplicates (IPv4 and IPv6 bindings).
pub fn published_ports(ports: Option<&[PortSummary]>) -> Vec<u16> {
    let published: BTreeSet<u16> = ports
        .unwrap_or_default()
        .iter()
        .filter(|port| matches!(port.typ, None | Some(PortSummaryTypeEnum::TCP)))
        .filter_map(|port| port.public_port.filter(|&public| public != 0))
        .collect();
    published.into_iter().collect()
}

/// The address a published port is opened at. Built from the port number
/// alone, so nothing a container says can make the app open anything else.
pub fn port_url(port: u16) -> String {
    format!("http://localhost:{port}")
}

/// The state of a container without the age in Docker's status text
/// (`Up 13 seconds`, `Exited (1) 2 hours ago`), which would go stale between
/// events. Keeps the parts that only change with an event: the exit code and
/// the health check result.
pub fn state_label(container: &ContainerSummary) -> String {
    let state = container_state(container);
    let status = container.status.as_deref().unwrap_or_default();
    let detail = if state == "exited" || state == "dead" {
        exit_code(status)
    } else {
        health(status)
    };
    match detail {
        Some(detail) => format!("{state} ({detail})"),
        None => state.to_string(),
    }
}

/// The code in `Exited (1) 2 hours ago` or `Dead (137) ...`.
fn exit_code(status: &str) -> Option<&str> {
    let rest = ["exited (", "dead ("].into_iter().find_map(|prefix| {
        let head = status.get(..prefix.len())?;
        head.eq_ignore_ascii_case(prefix)
            .then(|| &status[prefix.len()..])
    })?;
    let code = &rest[..rest.find(')')?];
    let digits = code.strip_prefix('-').unwrap_or(code);
    let is_number = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    is_number.then_some(code)
}

/// The health check result in `Up 5 minutes (unhealthy)`.
fn health(status: &str) -> Option<&'static str> {
    ["healthy", "unhealthy", "health: starting"]
        .into_iter()
        .find(|health| status.contains(&format!("({health})")))
}

/// The first 12 characters of an ID, without the digest prefix.
pub fn short_id(id: &str) -> String {
    head(id.strip_prefix("sha256:").unwrap_or(id), 12)
}

/// The Compose project a container belongs to, if any.
pub fn compose_project(container: &ContainerSummary) -> Option<&str> {
    let labels = container.labels.as_ref()?;
    let project = labels.get(COMPOSE_PROJECT_LABEL)?;
    (!project.is_empty()).then_some(project.as_str())
}

/// All tags, or the short ID for untagged images.
pub fn image_label(image: &ImageSummary) -> String {
    let tags: Vec<&str> = image
        .repo_tags
        .iter()
        .map(String::as_str)
        .filter(|tag| *tag != "<none>:<none>")
        .collect();
    if tags.is_empty() {
        short_id(&image.id)
    } else {
        tags.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn container(id: &str, names: &[&str]) -> ContainerSummary {
        ContainerSummary {
            id: Some(id.to_string()),
            names: (!names.is_empty()).then(|| names.iter().map(|name| name.to_string()).collect()),
            ..Default::default()
        }
    }

    fn with_state(state: ContainerSummaryStateEnum, status: &str) -> ContainerSummary {
        ContainerSummary {
            id: Some("a".to_string()),
            state: Some(state),
            status: Some(status.to_string()),
            ..Default::default()
        }
    }

    fn port(public: Option<u16>, typ: Option<PortSummaryTypeEnum>) -> PortSummary {
        PortSummary {
            ip: None,
            private_port: 80,
            public_port: public,
            typ,
        }
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1500), "1.5 KB");
        assert_eq!(format_bytes(412_000_000), "412 MB");
        assert_eq!(format_bytes(2_340_000_000), "2.3 GB");
        // The largest unit takes whatever is left.
        assert_eq!(format_bytes(5_000_000_000_000_000), "5000 TB");
    }

    #[test]
    fn formats_percentages_and_counts() {
        assert_eq!(format_percent(0.0), "0.0%");
        assert_eq!(format_percent(12.54), "12.5%");
        assert_eq!(format_percent(250.0), "250.0%");
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(50_000), "50,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn formats_dates_in_utc() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(86_399), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(1_700_000_000), "2023-11-14");
        assert_eq!(format_date(-1), "1969-12-31");
    }

    #[test]
    fn container_name_drops_the_leading_slash_and_falls_back_to_the_id() {
        assert_eq!(
            container_name(&container("abcdef0123456789", &["/web"])),
            "web"
        );
        assert_eq!(
            container_name(&container("abcdef0123456789", &[])),
            "abcdef012345"
        );
        assert_eq!(container_name(&ContainerSummary::default()), "");
    }

    #[test]
    fn published_ports_are_unique_sorted_and_tcp_only() {
        let ports = [
            port(Some(8080), Some(PortSummaryTypeEnum::TCP)),
            port(Some(8080), Some(PortSummaryTypeEnum::TCP)),
            port(Some(443), Some(PortSummaryTypeEnum::TCP)),
            port(Some(5353), Some(PortSummaryTypeEnum::UDP)),
            port(None, Some(PortSummaryTypeEnum::TCP)),
            // A port without a type is TCP, Docker's default.
            port(Some(9000), None),
        ];
        assert_eq!(published_ports(Some(&ports)), [443, 8080, 9000]);
        assert_eq!(published_ports(None), [] as [u16; 0]);
    }

    #[test]
    fn a_port_opens_on_localhost() {
        assert_eq!(port_url(8080), "http://localhost:8080");
    }

    #[test]
    fn short_id_strips_the_digest_prefix() {
        assert_eq!(short_id("sha256:0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short_id("0123"), "0123");
    }

    #[test]
    fn compose_project_comes_from_the_label() {
        let mut member = container("a", &[]);
        member.labels = Some(HashMap::from([(
            COMPOSE_PROJECT_LABEL.to_string(),
            "shop".to_string(),
        )]));
        assert_eq!(compose_project(&member), Some("shop"));
        assert_eq!(compose_project(&container("a", &[])), None);
        // An empty project name is no project.
        member.labels = Some(HashMap::from([(
            COMPOSE_PROJECT_LABEL.to_string(),
            String::new(),
        )]));
        assert_eq!(compose_project(&member), None);
    }

    #[test]
    fn state_label_shows_the_state_without_its_age() {
        use ContainerSummaryStateEnum::{DEAD, EXITED, RUNNING};
        assert_eq!(
            state_label(&with_state(RUNNING, "Up 13 seconds")),
            "running"
        );
        assert_eq!(
            state_label(&with_state(EXITED, "Exited (1) 2 hours ago")),
            "exited (1)"
        );
        assert_eq!(state_label(&with_state(EXITED, "")), "exited");
        assert_eq!(state_label(&with_state(DEAD, "Dead (-1)")), "dead (-1)");
        assert_eq!(
            state_label(&with_state(EXITED, "Exited (x) 2 hours ago")),
            "exited"
        );
        assert_eq!(
            state_label(&with_state(RUNNING, "Up 5 minutes (unhealthy)")),
            "running (unhealthy)"
        );
        assert_eq!(
            state_label(&with_state(RUNNING, "Up 5 minutes (healthy)")),
            "running (healthy)"
        );
        assert_eq!(
            state_label(&with_state(RUNNING, "Up 2 seconds (health: starting)")),
            "running (health: starting)"
        );
        assert_eq!(state_label(&container("a", &[])), "unknown");
        assert_eq!(
            state_label(&with_state(ContainerSummaryStateEnum::EMPTY, "")),
            "unknown"
        );
    }

    #[test]
    fn only_a_running_container_is_running() {
        use ContainerSummaryStateEnum::{PAUSED, RUNNING};
        assert!(is_running(&with_state(RUNNING, "")));
        assert!(!is_running(&with_state(PAUSED, "")));
        assert!(!is_running(&container("a", &[])));
    }

    fn image(id: &str, tags: &[&str]) -> ImageSummary {
        ImageSummary {
            id: id.to_string(),
            repo_tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn image_label_joins_tags() {
        assert_eq!(
            image_label(&image("sha256:abc", &["a:1", "a:2"])),
            "a:1, a:2"
        );
    }

    #[test]
    fn image_label_falls_back_to_the_short_id_for_untagged_images() {
        let untagged = image("sha256:0123456789abcdef", &["<none>:<none>"]);
        assert_eq!(image_label(&untagged), "0123456789ab");
        assert_eq!(
            image_label(&image("sha256:0123456789abcdef", &[])),
            "0123456789ab"
        );
    }
}
