//! Decoding of `wsl.exe` output.
//!
//! `wsl.exe` prints UTF-16LE by default and UTF-8 only when `WSL_UTF8=1` is
//! set. Docklet always sets the variable, but older WSL builds ignore it, so
//! the decoder detects UTF-16LE and falls back to it.

/// Decodes raw `wsl.exe` stdout/stderr bytes into a `String`.
pub fn decode(bytes: &[u8]) -> String {
    let text = if looks_like_utf16le(bytes) {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}').replace("\r\n", "\n")
}

/// UTF-8 text never contains NUL bytes, while UTF-16LE text made of mostly
/// ASCII or BMP characters almost always does (or starts with a BOM).
fn looks_like_utf16le(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    bytes.starts_with(&[0xff, 0xfe]) || bytes.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn decodes_utf8() {
        assert_eq!(
            decode("WSL 버전: 3.0.5.0\r\n".as_bytes()),
            "WSL 버전: 3.0.5.0\n"
        );
    }

    #[test]
    fn decodes_utf16le() {
        assert_eq!(
            decode(&utf16le("WSL 버전: 3.0.5.0\r\n")),
            "WSL 버전: 3.0.5.0\n"
        );
    }

    #[test]
    fn strips_utf16_bom() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(utf16le("docklet\r\n"));
        assert_eq!(decode(&bytes), "docklet\n");
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(decode(b""), "");
    }
}
