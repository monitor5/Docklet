//! Colour runs of a log line that carries ANSI escape sequences.
//!
//! Works on UTF-16, the form the paint path already holds, and writes into a
//! caller's vector so that painting allocates nothing once it has grown.

/// A foreground colour as the line asks for it; the theme turns it into RGB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    /// One of the 256 xterm colours; 0 to 15 come from the theme's palette.
    Index(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bold: bool,
}

/// A run of visible text, as a range of the line, in one style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

const ESC: u16 = 0x1b;
const BEL: u16 = 0x07;

/// Most SGR parameters one sequence may carry; the rest are ignored.
const MAX_PARAMS: usize = 16;

/// Splits `line` into visible runs. Escape sequences belong to no run.
pub fn parse(line: &[u16], spans: &mut Vec<Span>) {
    spans.clear();
    let mut style = Style::default();
    let mut start = 0;
    let mut index = 0;
    while index < line.len() {
        if line[index] != ESC {
            index += 1;
            continue;
        }
        if index > start {
            spans.push(Span {
                start,
                end: index,
                style,
            });
        }
        let (next, sgr) = scan_escape(line, index);
        if let Some(params) = sgr {
            apply_sgr(params, &mut style);
        }
        index = next;
        start = next;
    }
    if start < line.len() {
        spans.push(Span {
            start,
            end: line.len(),
            style,
        });
    }
}

/// Appends the visible text of `line`, without escape sequences, to `out`.
pub fn strip_into(line: &[u16], spans: &mut Vec<Span>, out: &mut Vec<u16>) {
    parse(line, spans);
    for span in spans.iter() {
        out.extend_from_slice(&line[span.start..span.end]);
    }
}

/// Reads the escape sequence that starts at `at` (an ESC). Returns the index
/// after it and, for a colour sequence (`ESC [ ... m`), its parameters.
fn scan_escape(line: &[u16], at: usize) -> (usize, Option<&[u16]>) {
    let Some(&kind) = line.get(at + 1) else {
        return (line.len(), None);
    };
    match kind {
        // CSI: parameters 0x30-0x3F, intermediates 0x20-0x2F, one final byte.
        0x5b => {
            let mut index = at + 2;
            while line
                .get(index)
                .is_some_and(|&unit| (0x30..=0x3f).contains(&unit))
            {
                index += 1;
            }
            let params_end = index;
            while line
                .get(index)
                .is_some_and(|&unit| (0x20..=0x2f).contains(&unit))
            {
                index += 1;
            }
            match line.get(index) {
                Some(&last) if (0x40..=0x7e).contains(&last) => {
                    let colour = last == u16::from(b'm') && params_end == index;
                    (index + 1, colour.then(|| &line[at + 2..params_end]))
                }
                // Unterminated or broken: hide the rest rather than show junk.
                _ => (line.len().min(index.max(at + 2)), None),
            }
        }
        // OSC (window titles, hyperlinks): ends at BEL or ESC \.
        0x5d => {
            let mut index = at + 2;
            while index < line.len() {
                if line[index] == BEL {
                    return (index + 1, None);
                }
                if line[index] == ESC && line.get(index + 1) == Some(&u16::from(b'\\')) {
                    return (index + 2, None);
                }
                index += 1;
            }
            (line.len(), None)
        }
        // Two-byte sequences such as `ESC ( B`, or a stray ESC.
        0x20..=0x2f => (line.len().min(at + 3), None),
        _ => (at + 2, None),
    }
}

/// The numbers of an SGR sequence: `;` and `:` separate, empty means 0.
fn params(raw: &[u16], out: &mut [u32; MAX_PARAMS]) -> usize {
    let mut count = 0;
    let mut value = 0u32;
    for &unit in raw {
        match unit {
            0x30..=0x39 => {
                value = value
                    .saturating_mul(10)
                    .saturating_add(u32::from(unit - 0x30))
            }
            0x3a | 0x3b => {
                if count < MAX_PARAMS {
                    out[count] = value;
                    count += 1;
                }
                value = 0;
            }
            // `?` and the like mark private sequences; they set no colour.
            _ => return 0,
        }
    }
    if count < MAX_PARAMS {
        out[count] = value;
        count += 1;
    }
    count
}

fn apply_sgr(raw: &[u16], style: &mut Style) {
    let mut values = [0u32; MAX_PARAMS];
    let count = params(raw, &mut values);
    let values = &values[..count];
    let mut index = 0;
    while index < values.len() {
        let code = values[index];
        index += 1;
        match code {
            0 => *style = Style::default(),
            1 => style.bold = true,
            22 => style.bold = false,
            30..=37 => style.fg = Color::Index((code - 30) as u8),
            90..=97 => style.fg = Color::Index((code - 90 + 8) as u8),
            39 => style.fg = Color::Default,
            38 | 48 => {
                let (color, used) = extended(&values[index..]);
                index += used;
                if code == 38
                    && let Some(color) = color
                {
                    style.fg = color;
                }
            }
            // Backgrounds, underline, inverse and the rest are not drawn.
            _ => {}
        }
    }
}

/// Reads `5;n` or `2;r;g;b` after a 38 or 48. Returns the colour and how
/// many values it used.
fn extended(values: &[u32]) -> (Option<Color>, usize) {
    let byte = |value: u32| value.min(255) as u8;
    match values {
        [5, n, ..] => (Some(Color::Index(byte(*n))), 2),
        [2, r, g, b, ..] => (Some(Color::Rgb(byte(*r), byte(*g), byte(*b))), 4),
        [5] | [2, ..] => (None, values.len()),
        _ => (None, 0),
    }
}

/// RGB of xterm colours 16 to 255: a 6×6×6 cube, then 24 greys.
pub fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => (0, 0, 0),
        16..=231 => {
            let n = index - 16;
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let grey = 8 + (index - 232) * 10;
            (grey, grey, grey)
        }
    }
}

/// Bold text with one of the eight basic colours uses its bright variant, as
/// terminals do.
pub fn effective_index(style: Style) -> Option<u8> {
    match style.fg {
        Color::Index(n) if style.bold && n < 8 => Some(n + 8),
        Color::Index(n) => Some(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn runs(text: &str) -> Vec<(String, Style)> {
        let line = utf16(text);
        let mut spans = Vec::new();
        parse(&line, &mut spans);
        spans
            .iter()
            .map(|span| {
                (
                    String::from_utf16_lossy(&line[span.start..span.end]),
                    span.style,
                )
            })
            .collect()
    }

    fn fg(index: u8) -> Style {
        Style {
            fg: Color::Index(index),
            bold: false,
        }
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(
            runs("hello 한글"),
            vec![("hello 한글".into(), Style::default())]
        );
        assert!(runs("").is_empty());
    }

    #[test]
    fn basic_colours_split_the_line_and_reset() {
        assert_eq!(
            runs("a \x1b[32mINFO\x1b[0m b"),
            vec![
                ("a ".into(), Style::default()),
                ("INFO".into(), fg(2)),
                (" b".into(), Style::default()),
            ]
        );
        assert_eq!(runs("\x1b[91mx\x1b[39my")[0].1, fg(9));
        assert_eq!(runs("\x1b[91mx\x1b[39my")[1].1, Style::default());
    }

    #[test]
    fn bold_and_colour_combine_in_one_sequence() {
        let style = runs("\x1b[1;31mE")[0].1;
        assert_eq!(
            style,
            Style {
                fg: Color::Index(1),
                bold: true
            }
        );
        assert_eq!(effective_index(style), Some(9));
        assert_eq!(runs("\x1b[1;31m\x1b[22mE")[0].1, fg(1));
    }

    #[test]
    fn empty_parameters_mean_reset() {
        assert_eq!(runs("\x1b[31mA\x1b[mB")[1].1, Style::default());
    }

    #[test]
    fn extended_colours_are_read_and_backgrounds_skipped() {
        assert_eq!(runs("\x1b[38;5;208mX")[0].1, fg(208));
        assert_eq!(runs("\x1b[38;2;1;2;3mX")[0].1.fg, Color::Rgb(1, 2, 3),);
        // The 48 consumes its own arguments, so the 32 after it still counts.
        assert_eq!(runs("\x1b[48;5;12;32mX")[0].1, fg(2));
        assert_eq!(runs("\x1b[48;2;9;9;9;33mX")[0].1, fg(3));
    }

    #[test]
    fn other_sequences_are_hidden_and_change_nothing() {
        assert_eq!(
            runs("a\x1b[2Kb"),
            vec![
                ("a".into(), Style::default()),
                ("b".into(), Style::default()),
            ]
        );
        assert_eq!(runs("\x1b[?25lx")[0], ("x".into(), Style::default()));
        assert_eq!(runs("\x1b]0;title\x07x")[0].0, "x");
        assert_eq!(runs("\x1b]8;;http://a\x1b\\x")[0].0, "x");
        assert_eq!(runs("\x1b(Bx")[0].0, "x");
    }

    #[test]
    fn broken_sequences_do_not_leak_or_panic() {
        assert_eq!(runs("a\x1b"), vec![("a".into(), Style::default())]);
        assert_eq!(runs("a\x1b["), vec![("a".into(), Style::default())]);
        assert_eq!(runs("a\x1b[31"), vec![("a".into(), Style::default())]);
        assert_eq!(runs("\x1b[99999999999999999999mx")[0].0, "x");
        let many = format!("\x1b[{}32mx", "1;".repeat(40));
        assert_eq!(runs(&many)[0].0, "x");
    }

    #[test]
    fn stripping_keeps_only_text() {
        let line = utf16("\x1b[33mWARN\x1b[0m disk 90%");
        let mut spans = Vec::new();
        let mut out = Vec::new();
        strip_into(&line, &mut spans, &mut out);
        assert_eq!(String::from_utf16_lossy(&out), "WARN disk 90%");
    }

    #[test]
    fn the_xterm_cube_and_greys() {
        assert_eq!(xterm_rgb(16), (0, 0, 0));
        assert_eq!(xterm_rgb(196), (255, 0, 0));
        assert_eq!(xterm_rgb(231), (255, 255, 255));
        assert_eq!(xterm_rgb(232), (8, 8, 8));
        assert_eq!(xterm_rgb(255), (238, 238, 238));
    }
}
