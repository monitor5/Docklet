//! A JSON writer for the `--json` output.
//!
//! The CLI only ever writes JSON, a few small objects per run, so it carries
//! this instead of `serde_json`. Measured on the release build: this writer
//! adds about 1.5 KB; `serde_json` adds the same when used as a bare
//! serializer and about 13 KB with `Value` or `json!`. So the dependency
//! would buy nothing, and it cannot write ASCII-only output without a custom
//! formatter.
//!
//! The output is pure ASCII: everything outside the printable ASCII range is
//! written as `\uXXXX`. Text from `wsl.exe` is localized, and a reader that
//! decodes the CLI's stdout with a legacy code page (Windows PowerShell does)
//! would otherwise receive mangled text, or even broken JSON.

use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(u64),
    Text(String),
    List(Vec<Json>),
    /// Fields are written in the order they were added.
    Object(Vec<(&'static str, Json)>),
}

impl Json {
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => {
                let _ = write!(out, "{value}");
            }
            Self::Text(text) => write_string(out, text),
            Self::List(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' '..='~' => out.push(c),
            // Control characters, DEL and everything beyond ASCII; characters
            // outside the BMP become a surrogate pair, as JSON requires.
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Builds an object field by field.
#[derive(Debug, Default)]
pub struct Object(Vec<(&'static str, Json)>);

impl Object {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, key: &'static str, value: impl Into<Json>) -> Self {
        self.0.push((key, value.into()));
        self
    }

    /// Adds the field only when there is a value; `set` with an `Option`
    /// writes `null` instead.
    pub fn set_some(self, key: &'static str, value: Option<impl Into<Json>>) -> Self {
        match value {
            Some(value) => self.set(key, value),
            None => self,
        }
    }
}

impl From<Object> for Json {
    fn from(object: Object) -> Self {
        Self::Object(object.0)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<u64> for Json {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

impl From<&str> for Json {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl From<String> for Json {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(items: Vec<T>) -> Self {
        Self::List(items.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> String {
        Json::from(value).render()
    }

    #[test]
    fn scalars() {
        assert_eq!(Json::Null.render(), "null");
        assert_eq!(Json::from(true).render(), "true");
        assert_eq!(Json::from(false).render(), "false");
        assert_eq!(Json::from(0u64).render(), "0");
        assert_eq!(Json::from(u64::MAX).render(), "18446744073709551615");
        assert_eq!(text(""), r#""""#);
        assert_eq!(text("running"), r#""running""#);
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(text(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(
            text(r"\\.\pipe\docklet_engine"),
            r#""\\\\.\\pipe\\docklet_engine""#
        );
        assert_eq!(text(r"C:\Users\me"), r#""C:\\Users\\me""#);
        // A backslash before a quote must not end up escaping the quote.
        assert_eq!(text(r#"\""#), r#""\\\"""#);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(text("a\nb\r\n\tc"), r#""a\nb\r\n\tc""#);
        assert_eq!(text("\u{0}"), r#""\u0000""#);
        assert_eq!(text("\u{8}\u{c}"), r#""\u0008\u000c""#);
        assert_eq!(text("\u{1b}[31m"), r#""\u001b[31m""#);
        assert_eq!(text("\u{1f}"), r#""\u001f""#);
        assert_eq!(text("\u{7f}"), r#""\u007f""#);
    }

    #[test]
    fn every_control_character_leaves_as_an_escape() {
        for code in 0u32..0x20 {
            let c = char::from_u32(code).unwrap();
            let rendered = text(&c.to_string());
            assert!(rendered.starts_with("\"\\"), "{code:#x}: {rendered}");
            assert!(rendered.bytes().all(|byte| (0x20..0x7f).contains(&byte)));
        }
    }

    #[test]
    fn printable_ascii_is_written_as_it_is() {
        let printable: String = (' '..='~').filter(|c| !matches!(c, '"' | '\\')).collect();
        assert_eq!(text(&printable), format!("\"{printable}\""));
        // `/` needs no escape and gets none; Docker host URLs stay readable.
        assert_eq!(
            text("npipe:////./pipe/docklet_engine"),
            r#""npipe:////./pipe/docklet_engine""#
        );
    }

    #[test]
    fn text_beyond_ascii_is_escaped_so_the_output_is_pure_ascii() {
        assert_eq!(text("é"), r#""\u00e9""#);
        // A localized wsl.exe message.
        assert_eq!(text("버전"), r#""\ubc84\uc804""#);
        assert_eq!(text("\u{2028}\u{feff}"), r#""\u2028\ufeff""#);
        // Outside the BMP: a surrogate pair.
        assert_eq!(text("🐳"), r#""\ud83d\udc33""#);
        assert_eq!(text("\u{10ffff}"), r#""\udbff\udfff""#);
        assert!(text("실행 중인 배포가 없습니다 🐳").is_ascii());
    }

    #[test]
    fn lists() {
        assert_eq!(Json::List(Vec::new()).render(), "[]");
        assert_eq!(Json::from(vec!["a", "b"]).render(), r#"["a","b"]"#);
        assert_eq!(
            Json::List(vec![Json::Null, Json::from(1u64), Json::List(Vec::new())]).render(),
            "[null,1,[]]"
        );
    }

    #[test]
    fn objects_keep_the_order_of_their_fields() {
        assert_eq!(Json::from(Object::new()).render(), "{}");
        let object = Object::new()
            .set("zebra", 1u64)
            .set("apple", "x")
            .set("nested", Object::new().set("ok", true));
        assert_eq!(
            Json::from(object).render(),
            r#"{"zebra":1,"apple":"x","nested":{"ok":true}}"#
        );
    }

    #[test]
    fn an_absent_value_is_null_with_set_and_left_out_with_set_some() {
        let none: Option<&str> = None;
        let object = Object::new()
            .set("reason", none)
            .set_some("hint", none)
            .set("version", Some("3.0.5"))
            .set_some("fix", Some("docklet up"));
        assert_eq!(
            Json::from(object).render(),
            r#"{"reason":null,"version":"3.0.5","fix":"docklet up"}"#
        );
    }

    #[test]
    fn keys_are_escaped_like_any_string() {
        let object = Object::new().set("a\"b", true);
        assert_eq!(Json::from(object).render(), r#"{"a\"b":true}"#);
    }

    #[test]
    fn output_is_one_line() {
        let object = Object::new().set("message", "line one\nline two\r\n");
        assert!(!Json::from(object).render().contains(['\n', '\r']));
    }
}
