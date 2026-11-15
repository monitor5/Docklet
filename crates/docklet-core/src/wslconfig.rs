//! Reading the user's `%USERPROFILE%\.wslconfig`. Docklet never writes it.

/// Shown when [`mirrored_networking`] is true.
pub const MIRRORED_NETWORKING_WARNING: &str = "warning: .wslconfig sets networkingMode=mirrored; published container ports rely on \
     WSL's NAT localhost forwarding and may not be reachable from Windows";

/// True when the `.wslconfig` text sets `[wsl2] networkingMode` to
/// `mirrored`. Sections, keys and values compare case-insensitively, `#` and
/// `;` start comments, and the last assignment wins, as in WSL itself.
pub fn mirrored_networking(wslconfig: &str) -> bool {
    let mut in_wsl2 = false;
    let mut mirrored = false;
    for line in wslconfig.lines() {
        let line = strip_comment(line.trim_start_matches('\u{feff}')).trim();
        if let Some(section) = line.strip_prefix('[') {
            in_wsl2 = section
                .strip_suffix(']')
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("wsl2"));
            continue;
        }
        if !in_wsl2 {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("networkingMode") {
            let value = value.trim().trim_matches('"').trim();
            mirrored = value.eq_ignore_ascii_case("mirrored");
        }
    }
    mirrored
}

fn strip_comment(line: &str) -> &str {
    match line.find(['#', ';']) {
        Some(at) => &line[..at],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored() {
        let cases = [
            "[wsl2]\nnetworkingMode=mirrored\n",
            // Case, spacing, CRLF, a BOM and quotes do not matter.
            "\u{feff}[WSL2]\r\n  NetworkingMode = Mirrored  \r\n",
            "[ wsl2 ]\nNETWORKINGMODE=\"MIRRORED\"\n",
            // Trailing comments are stripped.
            "[wsl2] # main section\nnetworkingMode=mirrored # needed for the VPN\n",
            // Other sections before and after do not interfere.
            "[experimental]\nautoMemoryReclaim=gradual\n[wsl2]\nnetworkingMode=mirrored\n",
            "[wsl2]\nnetworkingMode=mirrored\n[experimental]\nsparseVhd=true\n",
            // The last assignment wins, even across repeated sections.
            "[wsl2]\nnetworkingMode=nat\n[wsl2]\nnetworkingMode=mirrored\n",
        ];
        for text in cases {
            assert!(mirrored_networking(text), "{text:?}");
        }
    }

    #[test]
    fn not_mirrored() {
        let cases = [
            "",
            "[wsl2]\nmemory=8GB\n",
            "[wsl2]\nnetworkingMode=NAT\n",
            "[wsl2]\nnetworkingMode=mirrored\nnetworkingMode=nat\n",
            // Commented out.
            "[wsl2]\n# networkingMode=mirrored\n",
            "[wsl2]\n; networkingMode=mirrored\n",
            // Outside `[wsl2]`.
            "networkingMode=mirrored\n[wsl2]\nmemory=8GB\n",
            "[wsl2]\nmemory=8GB\n[experimental]\nnetworkingMode=mirrored\n",
            "[wsl2x]\nnetworkingMode=mirrored\n",
        ];
        for text in cases {
            assert!(!mirrored_networking(text), "{text:?}");
        }
    }
}
