//! The user's `%USERPROFILE%\.wslconfig`, as text.
//!
//! The file is global: it configures the one VM every WSL 2 distro of the
//! user runs in, not only Docklet's. Docklet reads it to warn about mirrored
//! networking, and adds a few keys that make that VM lighter (see
//! [`light_entries`]). Adding never changes what is already there: comments,
//! unknown sections and keys, their order and the line endings are kept, and
//! a key the user set wins over Docklet's value. Everything here is text in,
//! text out; reading and writing the file is `crate::wslconfig_file`'s job.

use std::time::{Duration, SystemTime};

/// Shown when [`mirrored_networking`] is true.
pub const MIRRORED_NETWORKING_WARNING: &str = "warning: .wslconfig sets networkingMode=mirrored; published container ports rely on \
     WSL's NAT localhost forwarding and may not be reachable from Windows";

/// The section every key Docklet writes lives in.
pub const WSL2: &str = "wsl2";
/// `[wsl2] guiApplications`: WSLg, the support for Linux GUI apps.
pub const GUI_APPLICATIONS: &str = "guiApplications";
/// `[wsl2] memory`: the most memory the VM may take.
pub const MEMORY: &str = "memory";

/// The comment Docklet puts above the keys it adds, so that someone who
/// opens the file knows where they came from.
pub const MARKER: &str = "# Added by Docklet; `docklet wslconfig revert` removes it.";

/// The command that undoes what Docklet wrote.
pub const UNDO: &str = "docklet wslconfig revert";

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

/// One `key=value` in one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Lower case, as Docklet compares sections.
    pub section: String,
    pub key: String,
    pub value: String,
}

impl Entry {
    pub fn new(section: &str, key: &str, value: &str) -> Self {
        Self {
            section: section.to_ascii_lowercase(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    /// `key=value`, as Docklet writes it.
    pub fn assignment(&self) -> String {
        format!("{}={}", self.key, self.value)
    }

    fn same_key(&self, other: &Entry) -> bool {
        self.section == other.section && self.key.eq_ignore_ascii_case(&other.key)
    }

    /// What the key costs the user, in a few words.
    pub fn cost(&self) -> String {
        if self.key.eq_ignore_ascii_case(GUI_APPLICATIONS) {
            "Linux GUI apps (WSLg) are off in every WSL distro".to_string()
        } else if self.key.eq_ignore_ascii_case(MEMORY) {
            format!(
                "the WSL VM, which every WSL distro shares, may use at most {}",
                self.value
            )
        } else {
            format!("{} applies to every WSL distro", self.assignment())
        }
    }
}

/// What "light" is: the keys Docklet wants in `.wslconfig`.
///
/// - `[wsl2] guiApplications=false` turns WSLg off. WSLg starts a system
///   distro with a Wayland and an X server and a PulseAudio server for
///   every VM; on the owner's PC it was about 390 MB of the VM's memory.
///   The cost: Linux GUI apps, and their sound, stop working in every
///   distro.
/// - `[wsl2] memory` only when the user asked for a cap (`vm-memory`): it
///   saved about 140 MB more, but limits every distro and can break large
///   builds.
///
/// Considered and left out: `[experimental] autoMemoryReclaim`, whose
/// default is already `dropCache`; `[experimental] sparseVhd`, which only
/// affects disks created later and saves disk, not memory; and
/// `[wsl2] vmIdleTimeout`, which would make the VM of every distro restart
/// more often to give its memory back a few seconds sooner.
pub fn light_entries(vm_memory: Option<&str>) -> Vec<Entry> {
    let mut entries = vec![Entry::new(WSL2, GUI_APPLICATIONS, "false")];
    if let Some(memory) = vm_memory {
        entries.push(Entry::new(WSL2, MEMORY, memory));
    }
    entries
}

/// How the file's bytes are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// The file's text and its encoding, or why Docklet must not touch it: it
/// is decoded only when encoding the text again gives the same bytes.
pub fn decode(bytes: &[u8]) -> Result<(String, Encoding), String> {
    let (text, encoding) = if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (utf8(rest)?, Encoding::Utf8Bom)
    } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (utf16(rest, u16::from_le_bytes)?, Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (utf16(rest, u16::from_be_bytes)?, Encoding::Utf16Be)
    } else {
        let text = utf8(bytes)?;
        if text.contains('\0') {
            return Err(
                "it contains NUL characters, which looks like UTF-16 without a byte order mark"
                    .to_string(),
            );
        }
        (text, Encoding::Utf8)
    };
    if encode(&text, encoding) != bytes {
        return Err("it would not be written back byte for byte".to_string());
    }
    Ok((text, encoding))
}

fn utf8(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        "it is neither UTF-8 nor UTF-16 with a byte order mark, so it cannot be edited safely"
            .to_string()
    })
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Result<String, String> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err("it is UTF-16 with an odd number of bytes".to_string());
    }
    let units: Vec<u16> = pairs.iter().map(|pair| unit(*pair)).collect();
    String::from_utf16(&units).map_err(|_| "it is not valid UTF-16".to_string())
}

/// The bytes of `text` in `encoding`, with its byte order mark if it has one.
pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [&[0xef, 0xbb, 0xbf], text.as_bytes()].concat(),
        Encoding::Utf16Le => [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect(),
        Encoding::Utf16Be => [0xfe, 0xff]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect(),
    }
}

/// What Docklet put into the file and still answers for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ownership {
    /// The keys Docklet added, with the value it wrote.
    pub entries: Vec<Entry>,
    /// Sections Docklet added because the file had none of that name.
    pub added_sections: Vec<String>,
}

/// What happened, or would happen, to one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    /// The key was missing; Docklet adds it.
    Added,
    /// Docklet's own line, with a value Docklet no longer wants.
    Updated,
    /// Docklet's own line, already as wanted.
    Unchanged,
    /// The user set the key to Docklet's value; it stays theirs.
    AlreadySet,
    /// The user set the key to another value, which wins.
    Kept,
    /// Docklet's own line, no longer wanted; it goes.
    Removed,
    /// Docklet wrote the key, but the user has changed or removed the line
    /// since. It is left alone and no longer Docklet's.
    Released,
}

impl KeyResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::AlreadySet => "already_set",
            Self::Kept => "kept",
            Self::Removed => "removed",
            Self::Released => "released",
        }
    }

    /// The file changes because of this key.
    pub fn changes_file(self) -> bool {
        matches!(self, Self::Added | Self::Updated | Self::Removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyReport {
    /// The key with Docklet's value: the one it wants, or the one it wrote.
    pub entry: Entry,
    pub result: KeyResult,
    /// The value in the file, where it is not Docklet's: for `Kept`,
    /// `AlreadySet` and `Released`, and the old value for `Updated`.
    pub file_value: Option<String>,
    /// Lines (from 1) that set the key in another section, or before any
    /// section, where WSL ignores it.
    pub misplaced: Vec<usize>,
}

/// The file's text after Docklet's change, and what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub text: String,
    pub ownership: Ownership,
    pub keys: Vec<KeyReport>,
}

/// Brings the text to `wanted`, touching only what Docklet owns.
///
/// For each wanted key: a key the user set (anything in `owned` is not the
/// user's) is left as it is, whatever its value; a missing key is added; a
/// line Docklet wrote gets the wanted value. Each owned key that is no longer
/// wanted is removed, if its line still holds the value Docklet wrote. A
/// line the user changed is theirs from then on. Once Docklet owns nothing,
/// its comment and any section it added and that is now empty go too.
///
/// `reconcile(text, &[], owned)` is the revert.
pub fn reconcile(text: &str, wanted: &[Entry], owned: &Ownership) -> Plan {
    let mut doc = Doc::parse(text);
    let mut ownership = owned.clone();
    let mut keys = Vec::new();

    for want in wanted {
        let misplaced = doc.misplaced(want);
        let mine = ownership
            .entries
            .iter()
            .position(|entry| entry.same_key(want));
        let report = |result, file_value: Option<String>| KeyReport {
            entry: want.clone(),
            result,
            file_value,
            misplaced: misplaced.clone(),
        };
        match (doc.effective(want), mine) {
            (Some(line), Some(index))
                if same_value(&doc.value(line), &ownership.entries[index].value) =>
            {
                let written = ownership.entries[index].value.clone();
                if same_value(&written, &want.value) {
                    keys.push(report(KeyResult::Unchanged, None));
                } else {
                    doc.set(line, want);
                    ownership.entries[index] = want.clone();
                    keys.push(report(KeyResult::Updated, Some(written)));
                }
            }
            (Some(line), mine) => {
                // The user's line, or Docklet's line the user has changed.
                if let Some(index) = mine {
                    ownership.entries.remove(index);
                }
                let value = doc.value(line);
                let result = if same_value(&value, &want.value) {
                    KeyResult::AlreadySet
                } else {
                    KeyResult::Kept
                };
                keys.push(report(result, Some(value)));
            }
            (None, mine) => {
                // Missing, or Docklet's line was deleted by hand: the
                // setting still asks for it, so it is written again.
                if let Some(index) = mine {
                    ownership.entries.remove(index);
                }
                if doc.add(want) {
                    ownership.added_sections.push(want.section.clone());
                }
                ownership.entries.push(want.clone());
                keys.push(report(KeyResult::Added, None));
            }
        }
    }

    let unwanted: Vec<Entry> = ownership
        .entries
        .iter()
        .filter(|entry| !wanted.iter().any(|want| want.same_key(entry)))
        .cloned()
        .collect();
    for entry in unwanted {
        ownership.entries.retain(|owned| !owned.same_key(&entry));
        let (result, file_value) = match doc.effective(&entry) {
            Some(line) if same_value(&doc.value(line), &entry.value) => {
                doc.remove(line);
                (KeyResult::Removed, None)
            }
            Some(line) => (KeyResult::Released, Some(doc.value(line))),
            None => (KeyResult::Released, None),
        };
        keys.push(KeyReport {
            entry,
            result,
            file_value,
            misplaced: Vec::new(),
        });
    }

    if ownership.entries.is_empty() {
        doc.remove_markers();
        for section in std::mem::take(&mut ownership.added_sections) {
            doc.remove_empty_section(&section);
        }
    }
    Plan {
        text: doc.render(),
        ownership,
        keys,
    }
}

fn same_value(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// One line and what ended it: `"\r\n"`, `"\n"`, or nothing on a last line
/// without a line break.
#[derive(Debug, Clone)]
struct Line {
    text: String,
    ending: &'static str,
}

enum Kind<'a> {
    Section(&'a str),
    Assignment { key: &'a str, value: &'a str },
    Other,
}

fn kind(text: &str) -> Kind<'_> {
    let content = strip_comment(text.trim_start_matches('\u{feff}')).trim();
    if let Some(rest) = content.strip_prefix('[') {
        return match rest.strip_suffix(']') {
            Some(name) => Kind::Section(name.trim()),
            None => Kind::Other,
        };
    }
    match content.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() => Kind::Assignment {
            key: key.trim(),
            value: value.trim().trim_matches('"').trim(),
        },
        _ => Kind::Other,
    }
}

/// The file as lines, edited in place. Lines Docklet does not touch are
/// rendered back exactly as they were read.
struct Doc {
    lines: Vec<Line>,
    /// The line break new lines get: the file's own, CRLF for a file that
    /// has none yet.
    newline: &'static str,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let lines = text
            .split_inclusive('\n')
            .map(|piece| {
                let (text, ending) = if let Some(text) = piece.strip_suffix("\r\n") {
                    (text, "\r\n")
                } else if let Some(text) = piece.strip_suffix('\n') {
                    (text, "\n")
                } else {
                    (piece, "")
                };
                Line {
                    text: text.to_string(),
                    ending,
                }
            })
            .collect();
        let newline = if text.contains("\r\n") || !text.contains('\n') {
            "\r\n"
        } else {
            "\n"
        };
        Self { lines, newline }
    }

    fn render(&self) -> String {
        self.lines
            .iter()
            .flat_map(|line| [line.text.as_str(), line.ending])
            .collect()
    }

    /// The section each line is in, lower case; `None` before the first
    /// header. A header is in its own section.
    fn sections(&self) -> Vec<Option<String>> {
        let mut current = None;
        self.lines
            .iter()
            .map(|line| {
                if let Kind::Section(name) = kind(&line.text) {
                    current = Some(name.to_ascii_lowercase());
                }
                current.clone()
            })
            .collect()
    }

    fn key_at(&self, line: usize) -> Option<&str> {
        match kind(&self.lines[line].text) {
            Kind::Assignment { key, .. } => Some(key),
            _ => None,
        }
    }

    /// The line WSL takes the key from: the last one in a section of that
    /// name, wherever the section is repeated.
    fn effective(&self, entry: &Entry) -> Option<usize> {
        let sections = self.sections();
        (0..self.lines.len()).rev().find(|&line| {
            sections[line].as_deref() == Some(entry.section.as_str())
                && self
                    .key_at(line)
                    .is_some_and(|key| key.eq_ignore_ascii_case(&entry.key))
        })
    }

    fn misplaced(&self, entry: &Entry) -> Vec<usize> {
        let sections = self.sections();
        (0..self.lines.len())
            .filter(|&line| {
                sections[line].as_deref() != Some(entry.section.as_str())
                    && self
                        .key_at(line)
                        .is_some_and(|key| key.eq_ignore_ascii_case(&entry.key))
            })
            .map(|line| line + 1)
            .collect()
    }

    fn value(&self, line: usize) -> String {
        match kind(&self.lines[line].text) {
            Kind::Assignment { value, .. } => value.to_string(),
            _ => String::new(),
        }
    }

    /// Rewrites a line Docklet wrote.
    fn set(&mut self, line: usize, entry: &Entry) {
        self.lines[line].text = entry.assignment();
    }

    fn remove(&mut self, line: usize) {
        self.lines.remove(line);
    }

    fn new_line(&self, text: &str) -> Line {
        Line {
            text: text.to_string(),
            ending: self.newline,
        }
    }

    /// The line before `at` gets a line break if it lacks one, so that a
    /// line inserted after it starts on a line of its own.
    fn break_before(&mut self, at: usize) {
        let newline = self.newline;
        if let Some(line) = at
            .checked_sub(1)
            .and_then(|before| self.lines.get_mut(before))
            && line.ending.is_empty()
        {
            line.ending = newline;
        }
    }

    /// Adds the entry at the end of the last section of its name, after
    /// that section's last key, with Docklet's comment above it unless the
    /// section has it already. Returns true when the section itself had to
    /// be added, at the end of the file.
    fn add(&mut self, entry: &Entry) -> bool {
        let sections = self.sections();
        let header = (0..self.lines.len()).rev().find(|&line| {
            matches!(kind(&self.lines[line].text), Kind::Section(_))
                && sections[line].as_deref() == Some(entry.section.as_str())
        });
        let Some(header) = header else {
            let at = self.lines.len();
            self.break_before(at);
            if self
                .lines
                .last()
                .is_some_and(|line| !line.text.trim().is_empty())
            {
                self.lines.push(self.new_line(""));
            }
            let lines = [
                format!("[{}]", entry.section),
                MARKER.to_string(),
                entry.assignment(),
            ];
            for text in lines {
                self.lines.push(self.new_line(&text));
            }
            return true;
        };
        let end = (header + 1..self.lines.len())
            .find(|&line| matches!(kind(&self.lines[line].text), Kind::Section(_)))
            .unwrap_or(self.lines.len());
        // After the last key, or after Docklet's comment if that comes
        // later (its key was deleted by hand).
        let at = (header + 1..end)
            .rev()
            .find(|&line| self.key_at(line).is_some() || self.lines[line].text.trim() == MARKER)
            .map_or(header + 1, |line| line + 1);
        let marked = (header + 1..end).any(|line| self.lines[line].text.trim() == MARKER);
        self.break_before(at);
        let mut lines = Vec::new();
        if !marked {
            lines.push(self.new_line(MARKER));
        }
        lines.push(self.new_line(&entry.assignment()));
        self.lines.splice(at..at, lines);
        false
    }

    fn remove_markers(&mut self) {
        self.lines.retain(|line| line.text.trim() != MARKER);
    }

    /// Removes the headers of `section` that have nothing under them, with
    /// the blank line Docklet put before each.
    fn remove_empty_section(&mut self, section: &str) {
        let mut line = self.lines.len();
        while line > 0 {
            line -= 1;
            let Kind::Section(name) = kind(&self.lines[line].text) else {
                continue;
            };
            if !name.eq_ignore_ascii_case(section) {
                continue;
            }
            let empty = self.lines[line + 1..]
                .iter()
                .take_while(|next| !matches!(kind(&next.text), Kind::Section(_)))
                .all(|next| next.text.trim().is_empty());
            if !empty {
                continue;
            }
            let end = (line + 1..self.lines.len())
                .find(|&next| matches!(kind(&self.lines[next].text), Kind::Section(_)))
                .unwrap_or(self.lines.len());
            let start = if line > 0 && self.lines[line - 1].text.trim().is_empty() {
                line - 1
            } else {
                line
            };
            self.lines.drain(start..end);
            line = start;
        }
    }
}

/// Whether the WSL VM runs, and since when if that is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vm {
    /// No distro runs. WSL keeps the VM for `vmIdleTimeout` (a minute by
    /// default) after the last one stops, so a start within that minute
    /// still finds the old VM; [`Effect::Pending`] then shows on the next
    /// look.
    Stopped,
    Running {
        /// The running distros, Docklet's included.
        distros: Vec<String>,
        /// When the VM booted, from `/proc/uptime` in the docklet distro.
        /// Unknown when only other distros run: Docklet does not run
        /// anything in them.
        booted: Option<SystemTime>,
    },
}

impl Vm {
    /// The running distros other than `own`.
    pub fn others(&self, own: &str) -> Vec<String> {
        match self {
            Self::Stopped => Vec::new(),
            Self::Running { distros, .. } => distros
                .iter()
                .filter(|name| *name != own)
                .cloned()
                .collect(),
        }
    }
}

/// Whether the file as it is now is what the WSL VM runs with. WSL reads
/// `.wslconfig` only when its VM starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// The VM is not running; it reads the file when it next starts.
    NextStart,
    /// The VM started after the file was last written.
    InEffect,
    /// The VM started before the file was last written.
    Pending,
    /// The VM runs and when it started is not known.
    Unknown,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NextStart => "next_wsl_start",
            Self::InEffect => "in_effect",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        }
    }
}

/// Compares the VM's boot with the file's last write.
///
/// The boot time is the time now minus the VM's uptime. While the host
/// sleeps the VM's clock may stand still, which makes the boot look later
/// than it was: a file written shortly before a host sleep may then show as
/// in effect when it is not.
pub fn effect(vm: &Vm, modified: SystemTime) -> Effect {
    match vm {
        Vm::Stopped => Effect::NextStart,
        Vm::Running {
            booted: Some(booted),
            ..
        } if *booted >= modified => Effect::InEffect,
        Vm::Running {
            booted: Some(_), ..
        } => Effect::Pending,
        Vm::Running { booted: None, .. } => Effect::Unknown,
    }
}

/// The first number of `/proc/uptime`: seconds since the kernel booted.
pub fn parse_uptime(text: &str) -> Option<Duration> {
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// One line on when a change to the file takes effect. `own` is Docklet's
/// distro, which Docklet itself can stop and start.
pub fn effect_line(effect: Effect, vm: &Vm, own: &str) -> String {
    let others = vm.others(own);
    let restart = if others.is_empty() {
        "run `docklet stop`, wait a minute for the WSL VM to stop, then `docklet start`".to_string()
    } else {
        format!(
            "WSL is running now ({}); run `wsl --shutdown` when those distros are idle",
            others.join(", ")
        )
    };
    match effect {
        Effect::NextStart => {
            "It takes effect the next time WSL starts; no WSL distro is running now.".to_string()
        }
        Effect::InEffect => "It is in effect: the WSL VM started after the change.".to_string(),
        Effect::Pending => format!("It takes effect the next time WSL starts: {restart}."),
        Effect::Unknown => format!(
            "It takes effect the next time WSL starts; if the running VM started before \
             the change: {restart}."
        ),
    }
}

/// What `status --json` and `doctor` say about Docklet's keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The `wslconfig` setting is `keep`: Docklet changes nothing by itself.
    pub keep: bool,
    /// Docklet's keys as they are in the file now.
    pub written: Vec<Entry>,
    /// Keys the user set to another value than Docklet's, with theirs.
    pub kept: Vec<(Entry, String)>,
    /// Keys `docklet wslconfig apply` would add or change.
    pub missing: Vec<Entry>,
    /// Whether what Docklet wrote is in effect; `None` when nothing is
    /// written.
    pub effect: Option<Effect>,
    /// Running distros other than Docklet's.
    pub others: Vec<String>,
    /// Why the file cannot be changed, when it cannot.
    pub problem: Option<String>,
}

impl Status {
    /// Sorts what [`reconcile`] would do into the status. `keys` comes from
    /// a plan for the wanted keys.
    pub fn from_keys(keys: &[KeyReport]) -> Self {
        let mut status = Self::default();
        for key in keys {
            match key.result {
                KeyResult::Unchanged | KeyResult::Removed => status.written.push(key.entry.clone()),
                KeyResult::Added | KeyResult::Updated => status.missing.push(key.entry.clone()),
                KeyResult::Kept => status.kept.push((
                    key.entry.clone(),
                    key.file_value.clone().unwrap_or_default(),
                )),
                KeyResult::AlreadySet | KeyResult::Released => {}
            }
        }
        status
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

    fn light() -> Vec<Entry> {
        light_entries(None)
    }

    fn with_memory(memory: &str) -> Vec<Entry> {
        light_entries(Some(memory))
    }

    /// Applies, checks that applying again changes nothing, reverts, and
    /// returns the applied text with the plan.
    fn apply(text: &str, wanted: &[Entry]) -> Plan {
        let plan = reconcile(text, wanted, &Ownership::default());
        let again = reconcile(&plan.text, wanted, &plan.ownership);
        assert_eq!(again.text, plan.text, "applying twice changed {text:?}");
        assert!(
            again.keys.iter().all(|key| !key.result.changes_file()),
            "{:?}",
            again.keys
        );
        plan
    }

    fn results(plan: &Plan) -> Vec<(&str, KeyResult)> {
        plan.keys
            .iter()
            .map(|key| (key.entry.key.as_str(), key.result))
            .collect()
    }

    const ADDED_TO_EMPTY: &str = "[wsl2]\r\n\
         # Added by Docklet; `docklet wslconfig revert` removes it.\r\n\
         guiApplications=false\r\n";

    #[test]
    fn the_text_added_to_an_empty_file() {
        let plan = apply("", &light());
        assert_eq!(plan.text, ADDED_TO_EMPTY);
        assert_eq!(results(&plan), [(GUI_APPLICATIONS, KeyResult::Added)]);
        assert_eq!(plan.ownership.entries, light());
        assert_eq!(plan.ownership.added_sections, ["wsl2"]);
        // Reverting leaves nothing behind.
        assert_eq!(reconcile(&plan.text, &[], &plan.ownership).text, "");
    }

    #[test]
    fn a_key_goes_after_the_last_key_of_the_existing_section() {
        let before = "# my settings\r\n[wsl2]\r\nmemory=8GB # plenty\r\n\r\n# the next one\r\n\
                      [experimental]\r\nsparseVhd=true\r\n";
        let plan = apply(before, &light());
        assert_eq!(
            plan.text,
            "# my settings\r\n[wsl2]\r\nmemory=8GB # plenty\r\n\
             # Added by Docklet; `docklet wslconfig revert` removes it.\r\n\
             guiApplications=false\r\n\r\n# the next one\r\n[experimental]\r\nsparseVhd=true\r\n"
        );
        assert!(plan.ownership.added_sections.is_empty());
        let reverted = reconcile(&plan.text, &[], &plan.ownership);
        assert_eq!(reverted.text, before);
        assert_eq!(results(&reverted), [(GUI_APPLICATIONS, KeyResult::Removed)]);
    }

    #[test]
    fn lf_files_get_lf_lines_and_crlf_files_crlf() {
        let plan = apply("[wsl2]\nkernel=C:\\\\k\n", &light());
        assert_eq!(
            plan.text,
            "[wsl2]\nkernel=C:\\\\k\n# Added by Docklet; `docklet wslconfig revert` removes it.\n\
             guiApplications=false\n"
        );
        let plan = apply("[wsl2]\r\nkernel=C:\\\\k\r\n", &light());
        assert!(
            plan.text
                .ends_with("removes it.\r\nguiApplications=false\r\n")
        );
        assert!(!plan.text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn a_last_line_without_a_break_gets_one_before_the_new_lines() {
        let plan = apply("[wsl2]\r\nmemory=8GB", &light());
        assert_eq!(
            plan.text,
            "[wsl2]\r\nmemory=8GB\r\n# Added by Docklet; `docklet wslconfig revert` removes \
             it.\r\nguiApplications=false\r\n"
        );
        let plan = apply("[experimental]\nsparseVhd=true", &light());
        assert_eq!(
            plan.text,
            "[experimental]\nsparseVhd=true\n\n[wsl2]\n# Added by Docklet; `docklet wslconfig \
             revert` removes it.\nguiApplications=false\n"
        );
        // The section Docklet added goes again on revert, with its blank line.
        let reverted = reconcile(&plan.text, &[], &plan.ownership);
        assert_eq!(reverted.text, "[experimental]\nsparseVhd=true\n");
    }

    #[test]
    fn a_key_the_user_set_wins_in_any_case_and_spelling() {
        for (text, value, result) in [
            ("[wsl2]\nguiApplications=true\n", "true", KeyResult::Kept),
            (
                "[WSL2]\nGUIAPPLICATIONS = True # I need gedit\n",
                "True",
                KeyResult::Kept,
            ),
            (
                "[wsl2]\nguiapplications=\"false\"\n",
                "false",
                KeyResult::AlreadySet,
            ),
            (
                "[ Wsl2 ]\r\nGuiApplications=FALSE\r\n",
                "FALSE",
                KeyResult::AlreadySet,
            ),
        ] {
            let plan = apply(text, &light());
            assert_eq!(plan.text, text);
            assert_eq!(results(&plan), [(GUI_APPLICATIONS, result)], "{text:?}");
            assert_eq!(plan.keys[0].file_value.as_deref(), Some(value));
            assert!(plan.ownership.entries.is_empty());
        }
    }

    #[test]
    fn the_last_of_repeated_keys_and_sections_is_the_one_that_counts() {
        let text =
            "[wsl2]\nguiApplications=false\n[experimental]\nx=1\n[wsl2]\nguiApplications=true\n";
        let plan = apply(text, &light());
        assert_eq!(plan.text, text);
        assert_eq!(plan.keys[0].result, KeyResult::Kept);
        assert_eq!(plan.keys[0].file_value.as_deref(), Some("true"));
        // A missing key goes into the last of the repeated sections.
        let text = "[wsl2]\nmemory=8GB\n[experimental]\nx=1\n[wsl2]\nswap=0\n[other]\ny=2\n";
        let plan = apply(text, &light());
        assert_eq!(
            plan.text,
            "[wsl2]\nmemory=8GB\n[experimental]\nx=1\n[wsl2]\nswap=0\n# Added by Docklet; \
             `docklet wslconfig revert` removes it.\nguiApplications=false\n[other]\ny=2\n"
        );
    }

    #[test]
    fn a_key_under_the_wrong_section_is_ignored_as_wsl_ignores_it_and_named() {
        let text = "guiApplications=true\n[experimental]\nguiApplications=true\n[wsl2]\nswap=0\n";
        let plan = apply(text, &light());
        assert_eq!(plan.keys[0].result, KeyResult::Added);
        assert_eq!(plan.keys[0].misplaced, [1, 3]);
        assert!(plan.text.ends_with("[wsl2]\nswap=0\n# Added by Docklet; `docklet wslconfig revert` removes it.\nguiApplications=false\n"));
        // Commented out is not set.
        let plan = apply("[wsl2]\n# guiApplications=true\n", &light());
        assert_eq!(plan.keys[0].result, KeyResult::Added);
        assert!(plan.keys[0].misplaced.is_empty());
    }

    #[test]
    fn a_bom_in_the_text_and_unknown_sections_are_kept() {
        let text = "\u{feff}[wsl2]\r\n[unknown]\r\nfoo = bar\r\n";
        let plan = apply(text, &light());
        assert_eq!(
            plan.text,
            "\u{feff}[wsl2]\r\n# Added by Docklet; `docklet wslconfig revert` removes it.\r\n\
             guiApplications=false\r\n[unknown]\r\nfoo = bar\r\n"
        );
        assert_eq!(reconcile(&plan.text, &[], &plan.ownership).text, text);
    }

    #[test]
    fn revert_after_the_user_edited_the_file_removes_only_docklets_keys() {
        let plan = apply("# mine\n[wsl2]\nswap=0\n", &with_memory("4GB"));
        assert_eq!(
            results(&plan),
            [
                (GUI_APPLICATIONS, KeyResult::Added),
                (MEMORY, KeyResult::Added)
            ]
        );
        // The user adds a key of their own, comments, and changes memory.
        let edited = plan
            .text
            .replace("memory=4GB", "memory=6GB")
            .replace("swap=0\n", "swap=0\nprocessors=4\n")
            + "[experimental]\nsparseVhd=true\n";
        let reverted = reconcile(&edited, &[], &plan.ownership);
        assert_eq!(
            reverted.text,
            "# mine\n[wsl2]\nswap=0\nprocessors=4\nmemory=6GB\n[experimental]\nsparseVhd=true\n"
        );
        assert_eq!(
            results(&reverted),
            [
                (GUI_APPLICATIONS, KeyResult::Removed),
                (MEMORY, KeyResult::Released)
            ]
        );
        assert_eq!(reverted.keys[1].file_value.as_deref(), Some("6GB"));
        assert!(reverted.ownership.entries.is_empty());
    }

    #[test]
    fn a_section_docklet_added_stays_when_the_user_put_keys_in_it() {
        let plan = apply("", &light());
        let edited = format!("{}swap=0\r\n", plan.text);
        let reverted = reconcile(&edited, &[], &plan.ownership);
        assert_eq!(reverted.text, "[wsl2]\r\nswap=0\r\n");
    }

    #[test]
    fn docklets_line_changed_by_the_user_becomes_theirs() {
        let plan = apply("", &light());
        let edited = plan
            .text
            .replace("guiApplications=false", "guiApplications=true");
        let again = reconcile(&edited, &light(), &plan.ownership);
        assert_eq!(again.keys[0].result, KeyResult::Kept);
        assert_eq!(again.text, edited.replace(&format!("{MARKER}\r\n"), ""));
        assert!(again.ownership.entries.is_empty());
    }

    #[test]
    fn docklets_line_deleted_by_hand_is_written_again_while_the_setting_asks_for_it() {
        let plan = apply("[wsl2]\nswap=0\n", &light());
        let edited = plan.text.replace("guiApplications=false\n", "");
        let again = reconcile(&edited, &light(), &plan.ownership);
        assert_eq!(again.keys[0].result, KeyResult::Added);
        assert_eq!(again.text, plan.text);
    }

    #[test]
    fn the_memory_cap_is_added_changed_and_removed_with_the_setting() {
        let plan = apply("[wsl2]\nswap=0\n", &with_memory("4GB"));
        assert!(plan.text.ends_with("guiApplications=false\nmemory=4GB\n"));
        // One comment for both keys.
        assert_eq!(plan.text.matches(MARKER).count(), 1);
        let changed = reconcile(&plan.text, &with_memory("8GB"), &plan.ownership);
        assert_eq!(
            results(&changed),
            [
                (GUI_APPLICATIONS, KeyResult::Unchanged),
                (MEMORY, KeyResult::Updated)
            ]
        );
        assert_eq!(changed.keys[1].file_value.as_deref(), Some("4GB"));
        assert!(changed.text.ends_with("memory=8GB\n"));
        let off = reconcile(&changed.text, &light(), &changed.ownership);
        assert_eq!(
            results(&off),
            [
                (GUI_APPLICATIONS, KeyResult::Unchanged),
                (MEMORY, KeyResult::Removed)
            ]
        );
        assert!(off.text.ends_with("guiApplications=false\n"));
        assert_eq!(off.ownership.entries, light());
    }

    #[test]
    fn a_memory_cap_the_user_set_is_kept() {
        let plan = apply("[wsl2]\nmemory=16GB\n", &with_memory("4GB"));
        assert_eq!(
            results(&plan),
            [
                (GUI_APPLICATIONS, KeyResult::Added),
                (MEMORY, KeyResult::Kept)
            ]
        );
        assert!(plan.text.contains("memory=16GB\n"));
        assert!(!plan.text.contains("4GB"));
    }

    #[test]
    fn nothing_to_do_changes_nothing() {
        for text in [
            "[wsl2]\nguiApplications=false\n",
            "[wsl2]\r\nguiApplications=true\r\nkernel=C:\\\\x",
        ] {
            let plan = reconcile(text, &light(), &Ownership::default());
            assert_eq!(plan.text, text);
            assert!(plan.keys.iter().all(|key| !key.result.changes_file()));
        }
        // Reverting with nothing owned changes nothing either.
        let text = "[wsl2]\nguiApplications=false\n";
        assert_eq!(reconcile(text, &[], &Ownership::default()).text, text);
    }

    #[test]
    fn docklets_comment_goes_only_with_its_last_key() {
        let plan = apply("[wsl2]\nswap=0\n", &with_memory("4GB"));
        let off = reconcile(&plan.text, &light(), &plan.ownership);
        assert_eq!(off.text.matches(MARKER).count(), 1);
        let reverted = reconcile(&off.text, &[], &off.ownership);
        assert_eq!(reverted.text, "[wsl2]\nswap=0\n");
    }

    #[test]
    fn utf8_with_and_without_bom_and_utf16_round_trip() {
        let text = "# é\r\n[wsl2]\r\nmemory=8GB\r\n";
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
        ] {
            let bytes = encode(text, encoding);
            assert_eq!(decode(&bytes), Ok((text.to_string(), encoding)));
        }
        assert_eq!(&encode("a", Encoding::Utf8Bom)[..3], &[0xef, 0xbb, 0xbf]);
        assert_eq!(encode("a", Encoding::Utf16Le), [0xff, 0xfe, b'a', 0]);
        assert_eq!(encode("a", Encoding::Utf16Be), [0xfe, 0xff, 0, b'a']);
        assert_eq!(decode(b""), Ok((String::new(), Encoding::Utf8)));
    }

    #[test]
    fn what_cannot_be_written_back_is_refused() {
        // Latin-1, not UTF-8.
        assert!(decode(b"# caf\xe9\n").is_err());
        // UTF-16 without a BOM.
        let bare: Vec<u8> = "[wsl2]".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(decode(&bare).unwrap_err().contains("UTF-16"));
        // An odd byte count, and a lone surrogate.
        assert!(decode(&[0xff, 0xfe, b'a']).is_err());
        assert!(decode(&[0xff, 0xfe, 0x00, 0xd8]).is_err());
    }

    #[test]
    fn the_light_keys_and_their_costs() {
        assert_eq!(
            light_entries(None),
            [Entry::new("wsl2", "guiApplications", "false")]
        );
        let entries = light_entries(Some("4GB"));
        assert_eq!(entries[1].assignment(), "memory=4GB");
        assert!(entries[0].cost().contains("every WSL distro"));
        assert!(entries[1].cost().contains("at most 4GB"));
    }

    #[test]
    fn status_sorts_the_keys() {
        let plan = reconcile(
            "[wsl2]\nmemory=16GB\n",
            &with_memory("4GB"),
            &Ownership::default(),
        );
        let status = Status::from_keys(&plan.keys);
        assert_eq!(status.missing, light());
        assert_eq!(status.kept.len(), 1);
        assert_eq!(status.kept[0].1, "16GB");
        assert!(status.written.is_empty());
        let applied = reconcile(&plan.text, &with_memory("4GB"), &plan.ownership);
        let status = Status::from_keys(&applied.keys);
        assert_eq!(status.written, light());
        assert!(status.missing.is_empty());
    }

    #[test]
    fn the_effect_follows_the_vm_boot() {
        let written = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let running = |booted: Option<u64>| Vm::Running {
            distros: vec!["docklet".to_string()],
            booted: booted.map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
        };
        assert_eq!(effect(&Vm::Stopped, written), Effect::NextStart);
        assert_eq!(effect(&running(Some(1_001)), written), Effect::InEffect);
        assert_eq!(effect(&running(Some(1_000)), written), Effect::InEffect);
        assert_eq!(effect(&running(Some(999)), written), Effect::Pending);
        assert_eq!(effect(&running(None), written), Effect::Unknown);
        let names: Vec<&str> = [
            Effect::NextStart,
            Effect::InEffect,
            Effect::Pending,
            Effect::Unknown,
        ]
        .map(Effect::as_str)
        .to_vec();
        assert_eq!(names, ["next_wsl_start", "in_effect", "pending", "unknown"]);
    }

    #[test]
    fn uptime_is_the_first_number() {
        assert_eq!(
            parse_uptime("12345.67 54321.00\n"),
            Some(Duration::from_secs_f64(12345.67))
        );
        assert_eq!(parse_uptime("3 1"), Some(Duration::from_secs(3)));
        for wrong in ["", "x 1", "-1 2", "NaN 1", "inf"] {
            assert_eq!(parse_uptime(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn the_effect_line_never_suggests_a_shutdown_with_only_docklet_running() {
        let only_docklet = Vm::Running {
            distros: vec!["docklet".to_string()],
            booted: None,
        };
        let line = effect_line(Effect::Pending, &only_docklet, "docklet");
        assert!(!line.contains("--shutdown"), "{line}");
        assert!(line.contains("`docklet stop`"), "{line}");
        let with_ubuntu = Vm::Running {
            distros: vec!["Ubuntu".to_string(), "docklet".to_string()],
            booted: None,
        };
        assert_eq!(
            effect_line(Effect::Pending, &with_ubuntu, "docklet"),
            "It takes effect the next time WSL starts: WSL is running now (Ubuntu); run \
             `wsl --shutdown` when those distros are idle."
        );
        assert_eq!(
            effect_line(Effect::NextStart, &Vm::Stopped, "docklet"),
            "It takes effect the next time WSL starts; no WSL distro is running now."
        );
        assert_eq!(with_ubuntu.others("docklet"), ["Ubuntu"]);
    }
}
