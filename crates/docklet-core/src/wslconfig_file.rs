//! Reading and writing `.wslconfig` for [`crate::wslconfig`].
//!
//! Every function takes the file's path and Docklet's state directory, so
//! tests run against a temporary directory; nothing here knows where the
//! real file is.
//!
//! - Before Docklet first changes the file it copies it to
//!   `<data dir>\wslconfig\wslconfig.backup`, and a record there says which
//!   keys it added. `revert` removes those keys and nothing else.
//! - When the file is still exactly what Docklet last wrote, `revert` puts
//!   the backup back byte for byte (or deletes the file Docklet created).
//! - The file is replaced atomically: written to a temporary file in the
//!   same directory, then renamed over it.
//! - A file that is a symbolic link, that is read-only, or whose encoding
//!   cannot be written back unchanged is left alone, and the outcome says why.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::wslconfig::{self, Encoding, Entry, KeyReport, KeyResult, Ownership};

/// Docklet's directory for this, inside its data directory.
pub const STATE_DIR: &str = "wslconfig";
const RECORD: &str = "record.txt";
const BACKUP: &str = "wslconfig.backup";
const WRITTEN: &str = "wslconfig.written";
/// Next to `.wslconfig`, so the rename that replaces it stays on one volume.
const TEMP_NAME: &str = ".wslconfig.docklet-tmp";

const RECORD_HEADER: &str = "# What Docklet added to .wslconfig. `docklet wslconfig revert` removes it. \
     Do not edit.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `%USERPROFILE%\.wslconfig`.
    pub file: PathBuf,
    /// `<data dir>\wslconfig`.
    pub state: PathBuf,
}

impl Paths {
    pub fn new(file: PathBuf, data_dir: &Path) -> Self {
        Self {
            file,
            state: data_dir.join(STATE_DIR),
        }
    }

    pub fn record(&self) -> PathBuf {
        self.state.join(RECORD)
    }

    /// The file as it was before Docklet's first change.
    pub fn backup(&self) -> PathBuf {
        self.state.join(BACKUP)
    }

    fn written(&self) -> PathBuf {
        self.state.join(WRITTEN)
    }
}

/// What is at the file's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Missing,
    Regular {
        read_only: bool,
    },
    Symlink,
    /// A directory or a device.
    Other,
}

pub fn file_state(path: &Path) -> io::Result<FileState> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(FileState::Symlink),
        Ok(meta) if meta.is_file() => Ok(FileState::Regular {
            read_only: meta.permissions().readonly(),
        }),
        Ok(_) => Ok(FileState::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(FileState::Missing),
        Err(err) => Err(err),
    }
}

/// Why Docklet must not write at the path, if it must not.
///
/// A link is not followed: replacing it would put a plain file where the
/// user keeps a link (to dotfiles, say), and writing through it would change
/// a file elsewhere. A read-only file was made so on purpose.
pub fn refusal(state: FileState) -> Option<&'static str> {
    match state {
        FileState::Missing | FileState::Regular { read_only: false } => None,
        FileState::Regular { read_only: true } => Some("it is read-only"),
        FileState::Symlink => Some("it is a symbolic link, which Docklet does not write through"),
        FileState::Other => Some("it is not a regular file"),
    }
}

/// What Docklet keeps about its change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// There was no `.wslconfig` before; revert deletes the file.
    pub created: bool,
    /// The file has not changed between Docklet's writes, so the backup is
    /// what revert may put back whole.
    pub restorable: bool,
    pub ownership: Ownership,
}

impl Record {
    /// Reads the record; lines it does not understand are skipped.
    pub fn parse(text: &str) -> Self {
        let mut record = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match name.trim() {
                "created" => record.created = value == "yes",
                "restorable" => record.restorable = value == "yes",
                "added-section" => record
                    .ownership
                    .added_sections
                    .push(value.to_ascii_lowercase()),
                "key" => {
                    let mut words = value.splitn(3, ' ');
                    if let (Some(section), Some(key), Some(written)) =
                        (words.next(), words.next(), words.next())
                    {
                        record
                            .ownership
                            .entries
                            .push(Entry::new(section, key, written.trim()));
                    }
                }
                _ => {}
            }
        }
        record
    }

    pub fn render(&self) -> String {
        let yes_no = |flag: bool| if flag { "yes" } else { "no" };
        let mut text = format!(
            "{RECORD_HEADER}\ncreated = {}\nrestorable = {}\n",
            yes_no(self.created),
            yes_no(self.restorable)
        );
        for section in &self.ownership.added_sections {
            text.push_str(&format!("added-section = {section}\n"));
        }
        for entry in &self.ownership.entries {
            text.push_str(&format!(
                "key = {} {} {}\n",
                entry.section, entry.key, entry.value
            ));
        }
        text
    }
}

/// What `apply` or `revert` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub keys: Vec<KeyReport>,
    /// The file was written or deleted.
    pub changed: bool,
    /// Why the file was left alone, when it was.
    pub refused: Option<String>,
    /// Revert put back the file from before Docklet's first change, byte
    /// for byte.
    pub restored: bool,
    /// Revert deleted the file Docklet had created.
    pub deleted: bool,
    /// When the file was last written, after the call; `None` without a file.
    pub modified: Option<SystemTime>,
}

impl Outcome {
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            refused: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// What the file holds and what `apply` would do to it, without changing
/// anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    pub keys: Vec<KeyReport>,
    pub refused: Option<String>,
    pub exists: bool,
    pub modified: Option<SystemTime>,
}

enum Current {
    Missing,
    File {
        bytes: Vec<u8>,
        text: String,
        encoding: Encoding,
    },
}

impl Current {
    fn text(&self) -> &str {
        match self {
            Self::Missing => "",
            Self::File { text, .. } => text,
        }
    }

    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::File { bytes, .. } => Some(bytes),
        }
    }

    fn encoding(&self) -> Encoding {
        match self {
            // A new file is UTF-8 without a BOM, which WSL reads.
            Self::Missing => Encoding::Utf8,
            Self::File { encoding, .. } => *encoding,
        }
    }
}

/// The file, or why it cannot be edited. With `writing`, a file that may
/// not be written counts as one that cannot be edited.
fn read_current(path: &Path, writing: bool) -> io::Result<Result<Current, String>> {
    let state = file_state(path)?;
    if let Some(reason) = refusal(state).filter(|_| writing || state == FileState::Other) {
        return Ok(Err(reason.to_string()));
    }
    if state == FileState::Missing {
        return Ok(Ok(Current::Missing));
    }
    let bytes = fs::read(path)?;
    Ok(match wslconfig::decode(&bytes) {
        Ok((text, encoding)) => Ok(Current::File {
            bytes,
            text,
            encoding,
        }),
        Err(reason) => Err(reason),
    })
}

pub fn load_record(paths: &Paths) -> io::Result<Option<Record>> {
    match fs::read_to_string(paths.record()) {
        Ok(text) => Ok(Some(Record::parse(&text))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// What `apply(paths, wanted)` would do, and Docklet's keys in the file.
pub fn inspect(paths: &Paths, wanted: &[Entry]) -> io::Result<Inspection> {
    let current = match read_current(&paths.file, false)? {
        Ok(current) => current,
        Err(reason) => {
            return Ok(Inspection {
                refused: Some(reason),
                exists: true,
                ..Inspection::default()
            });
        }
    };
    let record = load_record(paths)?.unwrap_or_default();
    let plan = wslconfig::reconcile(current.text(), wanted, &record.ownership);
    Ok(Inspection {
        keys: plan.keys,
        // Reading is fine; writing is what a read-only file or a link stops.
        refused: refusal(file_state(&paths.file)?).map(str::to_string),
        exists: !matches!(current, Current::Missing),
        modified: modified(&paths.file),
    })
}

/// Brings the file to `wanted` (see [`wslconfig::reconcile`]).
pub fn apply(paths: &Paths, wanted: &[Entry]) -> io::Result<Outcome> {
    let current = match read_current(&paths.file, true)? {
        Ok(current) => current,
        Err(reason) => return Ok(Outcome::refused(reason)),
    };
    let old = load_record(paths)?;
    // A file deleted since Docklet wrote it starts afresh.
    let old = old.filter(|_| !matches!(current, Current::Missing));
    let plan = wslconfig::reconcile(
        current.text(),
        wanted,
        &old.as_ref()
            .map(|record| record.ownership.clone())
            .unwrap_or_default(),
    );
    let changed = plan.text != current.text();
    let record = match &old {
        Some(old) => Record {
            created: old.created,
            // Edited by someone else since Docklet's last write: the backup
            // would undo those edits too, so it is no longer put back whole.
            restorable: old.restorable
                && current.bytes() == read_optional(&paths.written())?.as_deref(),
            ownership: plan.ownership.clone(),
        },
        None => Record {
            created: matches!(current, Current::Missing),
            restorable: true,
            ownership: plan.ownership.clone(),
        },
    };
    if !changed {
        save_record(paths, &record, None)?;
        return Ok(Outcome {
            keys: plan.keys,
            modified: modified(&paths.file),
            ..Outcome::default()
        });
    }
    if record.created && record.ownership.entries.is_empty() && plan.text.trim().is_empty() {
        // Docklet made the file and no longer wants anything in it.
        fs::remove_file(&paths.file)?;
        clear_record(paths)?;
        return Ok(Outcome {
            keys: plan.keys,
            changed: true,
            deleted: true,
            ..Outcome::default()
        });
    }
    fs::create_dir_all(&paths.state)?;
    if old.is_none()
        && let Some(bytes) = current.bytes()
    {
        // Before the first write, and only then: a backup taken later
        // would already hold Docklet's keys.
        fs::write(paths.backup(), bytes)?;
    }
    let bytes = wslconfig::encode(&plan.text, current.encoding());
    write_atomically(&paths.file, &bytes)?;
    if let Err(err) = save_record(paths, &record, Some(&bytes)) {
        // Without the record the change could not be undone; take it back.
        let _ = match current.bytes() {
            Some(before) => write_atomically(&paths.file, before),
            None => fs::remove_file(&paths.file),
        };
        return Err(err);
    }
    Ok(Outcome {
        keys: plan.keys,
        changed: true,
        modified: modified(&paths.file),
        ..Outcome::default()
    })
}

/// Removes what Docklet added. Nothing happens without a record.
pub fn revert(paths: &Paths) -> io::Result<Outcome> {
    let Some(record) = load_record(paths)? else {
        return Ok(Outcome {
            modified: modified(&paths.file),
            ..Outcome::default()
        });
    };
    let current = match read_current(&paths.file, true)? {
        Ok(current) => current,
        Err(reason) => return Ok(Outcome::refused(reason)),
    };
    let removed = |result| -> Vec<KeyReport> {
        record
            .ownership
            .entries
            .iter()
            .map(|entry| KeyReport {
                entry: entry.clone(),
                result,
                file_value: None,
                misplaced: Vec::new(),
            })
            .collect()
    };
    let mut outcome = Outcome::default();
    match &current {
        Current::Missing => outcome.keys = removed(KeyResult::Released),
        Current::File { bytes, text, .. } => {
            let untouched = read_optional(&paths.written())?.is_some_and(|w| w == *bytes);
            let backup = read_optional(&paths.backup())?;
            if record.restorable && untouched && (record.created || backup.is_some()) {
                match backup.filter(|_| !record.created) {
                    Some(backup) => {
                        write_atomically(&paths.file, &backup)?;
                        outcome.restored = true;
                    }
                    None => {
                        fs::remove_file(&paths.file)?;
                        outcome.deleted = true;
                    }
                }
                outcome.changed = true;
                outcome.keys = removed(KeyResult::Removed);
            } else {
                let plan = wslconfig::reconcile(text, &[], &record.ownership);
                if record.created && plan.text.trim().is_empty() {
                    fs::remove_file(&paths.file)?;
                    outcome.deleted = true;
                    outcome.changed = true;
                } else if plan.text != *text {
                    write_atomically(
                        &paths.file,
                        &wslconfig::encode(&plan.text, current.encoding()),
                    )?;
                    outcome.changed = true;
                }
                outcome.keys = plan.keys;
            }
        }
    }
    clear_record(paths)?;
    outcome.modified = modified(&paths.file);
    Ok(outcome)
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Saves the record and, when given, the bytes Docklet just wrote. A record
/// that owns nothing is no record: the change is complete undone or never
/// happened.
fn save_record(paths: &Paths, record: &Record, written: Option<&[u8]>) -> io::Result<()> {
    if record.ownership.entries.is_empty() && record.ownership.added_sections.is_empty() {
        return clear_record(paths);
    }
    fs::create_dir_all(&paths.state)?;
    if let Some(written) = written {
        fs::write(paths.written(), written)?;
    }
    fs::write(paths.record(), record.render())
}

/// The backup stays: it is the user's file from before, and costs nothing.
fn clear_record(paths: &Paths) -> io::Result<()> {
    for path in [paths.record(), paths.written()] {
        match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

/// Writes a temporary file next to `path` and renames it over `path`, so
/// that a crash leaves either the old file or the new one, never half.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = path.with_file_name(TEMP_NAME);
    let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wslconfig::{GUI_APPLICATIONS, MARKER, light_entries};

    /// A home directory and a data directory under the system's temp
    /// directory, removed when dropped.
    struct Sandbox {
        root: PathBuf,
        paths: Paths,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("docklet-wslconfig-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("home")).unwrap();
            let paths = Paths::new(root.join("home").join(".wslconfig"), &root.join("data"));
            Self { root, paths }
        }

        fn write(&self, bytes: &[u8]) {
            fs::write(&self.paths.file, bytes).unwrap();
        }

        fn read(&self) -> Vec<u8> {
            fs::read(&self.paths.file).unwrap()
        }

        fn text(&self) -> String {
            String::from_utf8(self.read()).unwrap()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            if let Ok(meta) = fs::metadata(&self.paths.file) {
                let mut permissions = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&self.paths.file, permissions);
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn light() -> Vec<Entry> {
        light_entries(None)
    }

    fn results(outcome: &Outcome) -> Vec<KeyResult> {
        outcome.keys.iter().map(|key| key.result).collect()
    }

    #[test]
    fn no_file_is_created_and_revert_deletes_it() {
        let sandbox = Sandbox::new("missing");
        let outcome = apply(&sandbox.paths, &light()).unwrap();
        assert!(outcome.changed);
        assert_eq!(results(&outcome), [KeyResult::Added]);
        assert!(sandbox.text().contains("guiApplications=false"));
        assert!(outcome.modified.is_some());
        // Nothing to back up, but the record says the file is Docklet's.
        assert!(!sandbox.paths.backup().exists());
        let record = load_record(&sandbox.paths).unwrap().unwrap();
        assert!(record.created);
        assert_eq!(record.ownership.entries, light());

        let reverted = revert(&sandbox.paths).unwrap();
        assert!(reverted.deleted);
        assert!(!sandbox.paths.file.exists());
        assert_eq!(load_record(&sandbox.paths).unwrap(), None);
        // A second revert has nothing to do.
        let again = revert(&sandbox.paths).unwrap();
        assert!(!again.changed && again.keys.is_empty());
    }

    #[test]
    fn an_empty_file_is_restored_empty() {
        let sandbox = Sandbox::new("empty");
        sandbox.write(b"");
        apply(&sandbox.paths, &light()).unwrap();
        assert_eq!(fs::read(sandbox.paths.backup()).unwrap(), b"");
        let reverted = revert(&sandbox.paths).unwrap();
        assert!(reverted.restored);
        assert_eq!(sandbox.read(), b"");
    }

    #[test]
    fn revert_restores_byte_for_byte_and_apply_puts_it_back() {
        let sandbox = Sandbox::new("bytes");
        // UTF-8 with a BOM, CRLF, no final line break, a custom kernel.
        let before =
            b"\xef\xbb\xbf# mine\r\n[wsl2]\r\nkernel=C:\\\\k\\\\bzImage\r\nswap=0".to_vec();
        sandbox.write(&before);
        let applied = apply(&sandbox.paths, &light()).unwrap();
        assert!(applied.changed);
        let after = sandbox.read();
        assert!(after.starts_with(b"\xef\xbb\xbf# mine\r\n"));
        assert!(after.ends_with(b"swap=0\r\n# Added by Docklet; `docklet wslconfig revert` removes it.\r\nguiApplications=false\r\n"));
        assert_eq!(fs::read(sandbox.paths.backup()).unwrap(), before);

        let reverted = revert(&sandbox.paths).unwrap();
        assert!(reverted.restored);
        assert_eq!(sandbox.read(), before);
        assert_eq!(results(&reverted), [KeyResult::Removed]);

        let again = apply(&sandbox.paths, &light()).unwrap();
        assert!(again.changed);
        assert_eq!(sandbox.read(), after);
        // Nothing left over next to the file.
        let names: Vec<_> = fs::read_dir(sandbox.paths.file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [".wslconfig"]);
    }

    #[test]
    fn utf16_is_written_back_as_utf16() {
        let sandbox = Sandbox::new("utf16");
        let before = wslconfig::encode("[wsl2]\r\nmemory=8GB\r\n", Encoding::Utf16Le);
        sandbox.write(&before);
        apply(&sandbox.paths, &light()).unwrap();
        let (text, encoding) = wslconfig::decode(&sandbox.read()).unwrap();
        assert_eq!(encoding, Encoding::Utf16Le);
        assert!(text.ends_with("guiApplications=false\r\n"));
        revert(&sandbox.paths).unwrap();
        assert_eq!(sandbox.read(), before);
    }

    #[test]
    fn revert_after_user_edits_removes_only_docklets_keys() {
        let sandbox = Sandbox::new("edited");
        sandbox.write(b"[wsl2]\nswap=0\n");
        apply(&sandbox.paths, &light()).unwrap();
        let edited = sandbox.text().replace("swap=0\n", "swap=0\nprocessors=2\n");
        sandbox.write(edited.as_bytes());
        let reverted = revert(&sandbox.paths).unwrap();
        assert!(!reverted.restored);
        assert!(reverted.changed);
        assert_eq!(sandbox.text(), "[wsl2]\nswap=0\nprocessors=2\n");
    }

    #[test]
    fn edits_between_two_applies_are_never_undone_by_the_backup() {
        let sandbox = Sandbox::new("between");
        sandbox.write(b"[wsl2]\nswap=0\n");
        apply(&sandbox.paths, &light()).unwrap();
        let edited = sandbox.text() + "[experimental]\nsparseVhd=true\n";
        sandbox.write(edited.as_bytes());
        // A second apply that changes the file, then a revert.
        apply(&sandbox.paths, &light_entries(Some("4GB"))).unwrap();
        let record = load_record(&sandbox.paths).unwrap().unwrap();
        assert!(!record.restorable);
        revert(&sandbox.paths).unwrap();
        assert_eq!(
            sandbox.text(),
            "[wsl2]\nswap=0\n[experimental]\nsparseVhd=true\n"
        );
    }

    #[test]
    fn a_key_the_user_set_is_kept_and_nothing_is_recorded() {
        let sandbox = Sandbox::new("kept");
        let before = b"[wsl2]\r\nguiApplications=true\r\n";
        sandbox.write(before);
        let outcome = apply(&sandbox.paths, &light()).unwrap();
        assert!(!outcome.changed);
        assert_eq!(results(&outcome), [KeyResult::Kept]);
        assert_eq!(outcome.keys[0].file_value.as_deref(), Some("true"));
        assert_eq!(sandbox.read(), before);
        assert_eq!(load_record(&sandbox.paths).unwrap(), None);
        assert!(!sandbox.paths.backup().exists());
    }

    #[test]
    fn a_read_only_file_is_left_alone() {
        let sandbox = Sandbox::new("readonly");
        let before = b"[wsl2]\nswap=0\n";
        sandbox.write(before);
        let mut permissions = fs::metadata(&sandbox.paths.file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&sandbox.paths.file, permissions).unwrap();
        let outcome = apply(&sandbox.paths, &light()).unwrap();
        assert_eq!(outcome.refused.as_deref(), Some("it is read-only"));
        assert!(!outcome.changed);
        assert_eq!(sandbox.read(), before);
        // Reading still works: the status can say what is missing.
        let inspection = inspect(&sandbox.paths, &light()).unwrap();
        assert_eq!(inspection.keys[0].result, KeyResult::Added);
        assert_eq!(inspection.refused.as_deref(), Some("it is read-only"));
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let sandbox = Sandbox::new("symlink");
        let target = sandbox.root.join("dotfiles.wslconfig");
        fs::write(&target, b"[wsl2]\nswap=0\n").unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &sandbox.paths.file);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &sandbox.paths.file);
        // Windows lets only administrators and developer mode make links.
        if linked.is_err() {
            assert_eq!(
                refusal(FileState::Symlink),
                Some("it is a symbolic link, which Docklet does not write through")
            );
            return;
        }
        let outcome = apply(&sandbox.paths, &light()).unwrap();
        assert!(outcome.refused.unwrap().contains("symbolic link"));
        assert_eq!(fs::read(&target).unwrap(), b"[wsl2]\nswap=0\n");
        assert!(
            fs::symlink_metadata(&sandbox.paths.file)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn what_is_refused_and_why() {
        assert_eq!(refusal(FileState::Missing), None);
        assert_eq!(refusal(FileState::Regular { read_only: false }), None);
        assert_eq!(
            refusal(FileState::Regular { read_only: true }),
            Some("it is read-only")
        );
        assert!(refusal(FileState::Symlink).is_some());
        assert!(refusal(FileState::Other).is_some());
    }

    #[test]
    fn a_file_in_an_unknown_encoding_is_left_alone() {
        let sandbox = Sandbox::new("latin1");
        sandbox.write(b"# caf\xe9\n[wsl2]\n");
        let outcome = apply(&sandbox.paths, &light()).unwrap();
        assert!(outcome.refused.unwrap().contains("UTF-8"));
        assert_eq!(sandbox.read(), b"# caf\xe9\n[wsl2]\n");
        assert_eq!(load_record(&sandbox.paths).unwrap(), None);
    }

    #[test]
    fn a_file_deleted_since_docklet_wrote_it_starts_afresh() {
        let sandbox = Sandbox::new("deleted");
        sandbox.write(b"[wsl2]\nswap=0\n");
        apply(&sandbox.paths, &light()).unwrap();
        fs::remove_file(&sandbox.paths.file).unwrap();
        apply(&sandbox.paths, &light()).unwrap();
        assert!(load_record(&sandbox.paths).unwrap().unwrap().created);
        // Revert deletes what Docklet created rather than bring back the
        // file the user deleted.
        revert(&sandbox.paths).unwrap();
        assert!(!sandbox.paths.file.exists());
    }

    #[test]
    fn removing_the_last_key_ends_the_record() {
        let sandbox = Sandbox::new("unwanted");
        apply(&sandbox.paths, &light()).unwrap();
        let outcome = apply(&sandbox.paths, &[]).unwrap();
        assert_eq!(results(&outcome), [KeyResult::Removed]);
        assert!(outcome.deleted);
        assert_eq!(load_record(&sandbox.paths).unwrap(), None);
        assert!(!sandbox.paths.file.exists());
        // In a file of the user's, only Docklet's lines go.
        sandbox.write(b"[wsl2]\nswap=0\n");
        apply(&sandbox.paths, &light()).unwrap();
        apply(&sandbox.paths, &[]).unwrap();
        assert_eq!(sandbox.text(), "[wsl2]\nswap=0\n");
        assert!(!sandbox.text().contains(MARKER));
    }

    #[test]
    fn the_record_reads_back() {
        let record = Record {
            created: false,
            restorable: true,
            ownership: Ownership {
                entries: light_entries(Some("4GB")),
                added_sections: vec!["wsl2".to_string()],
            },
        };
        let text = record.render();
        assert!(text.contains("key = wsl2 guiApplications false\n"));
        assert!(text.contains("key = wsl2 memory 4GB\n"));
        assert_eq!(Record::parse(&text), record);
        // Unknown lines are skipped.
        assert_eq!(
            Record::parse("future = 1\nkey = wsl2\ncreated = yes\n"),
            Record {
                created: true,
                ..Record::default()
            }
        );
    }

    #[test]
    fn inspect_changes_nothing() {
        let sandbox = Sandbox::new("inspect");
        let inspection = inspect(&sandbox.paths, &light()).unwrap();
        assert!(!inspection.exists);
        assert_eq!(inspection.keys[0].result, KeyResult::Added);
        assert!(!sandbox.paths.file.exists());
        assert!(!sandbox.paths.state.exists());
        apply(&sandbox.paths, &light()).unwrap();
        let inspection = inspect(&sandbox.paths, &light()).unwrap();
        assert_eq!(inspection.keys[0].result, KeyResult::Unchanged);
        assert_eq!(inspection.keys[0].entry.key, GUI_APPLICATIONS);
    }
}
