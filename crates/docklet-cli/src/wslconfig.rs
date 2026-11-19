//! `.wslconfig` in the CLI: what `install`, `up`, `uninstall`,
//! `config set vm-memory` and `docklet wslconfig` do to it, and how they say
//! so. The file handling itself is `docklet_core::wslconfig_file`.

use std::time::SystemTime;

use docklet_core::engine::DISTRO_NAME;
use docklet_core::settings::{MemorySize, Settings, WslconfigMode};
use docklet_core::wsl::{Wsl, WslRunner};
use docklet_core::wslconfig::{self, Effect, KeyReport, KeyResult, Status, UNDO, Vm};
use docklet_core::wslconfig_file::{self, Outcome};

use crate::json::{Json, Object};

/// What an automatic change (by `install`, `up`, `uninstall`) came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auto {
    /// `"flag"` (`--no-wslconfig`) or `"setting"` (`wslconfig = keep`).
    Skipped(&'static str),
    /// The file could not be read or written.
    Failed {
        path: String,
        error: String,
    },
    Done {
        path: String,
        outcome: Outcome,
    },
}

impl Auto {
    pub fn changed(&self) -> bool {
        matches!(self, Self::Done { outcome, .. } if outcome.changed)
    }

    fn outcome(&self) -> Option<&Outcome> {
        match self {
            Self::Done { outcome, .. } => Some(outcome),
            _ => None,
        }
    }
}

/// Brings the file to what the settings ask for, unless `skip_flag` or the
/// `wslconfig` setting says to leave it alone. `revert` takes Docklet's keys
/// out instead (`uninstall`).
pub fn auto(settings: &Settings, skip_flag: bool, revert: bool) -> Auto {
    if skip_flag {
        return Auto::Skipped("flag");
    }
    if settings.wslconfig == WslconfigMode::Keep {
        return Auto::Skipped("setting");
    }
    let paths = match docklet_wsl::wslconfig_paths() {
        Ok(paths) => paths,
        Err(err) => {
            return Auto::Failed {
                path: ".wslconfig".to_string(),
                error: err.to_string(),
            };
        }
    };
    let path = paths.file.display().to_string();
    let result = if revert {
        wslconfig_file::revert(&paths)
    } else {
        wslconfig_file::apply(&paths, &settings.wslconfig_entries())
    };
    match result {
        Ok(outcome) => Auto::Done { path, outcome },
        Err(err) => Auto::Failed {
            path,
            error: err.to_string(),
        },
    }
}

/// Whether a change that was just made is in effect, when one was made.
/// Asks `wsl.exe` only then.
pub fn effect_of_change<R: WslRunner>(
    wsl: &Wsl<R>,
    outcome: Option<&Outcome>,
) -> Option<(Effect, Vm)> {
    let outcome = outcome.filter(|outcome| outcome.changed)?;
    let vm = docklet_wsl::vm_state(wsl).ok()?;
    // A deleted file has no time of its own; it changed just now.
    let modified = outcome.modified.unwrap_or_else(SystemTime::now);
    Some((wslconfig::effect(&vm, modified), vm))
}

pub fn effect_of(auto: &Auto, wsl: &Wsl<impl WslRunner>) -> Option<(Effect, Vm)> {
    effect_of_change(wsl, auto.outcome())
}

/// Lines for people: on stdout, and warnings for stderr.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Lines {
    pub out: Vec<String>,
    pub warnings: Vec<String>,
}

/// What an automatic change says. Quiet when nothing changed, unless
/// `verbose` (an install, where the user meets Docklet's keys first): then
/// a key of the user's that wins is named too.
pub fn auto_lines(auto: &Auto, verbose: bool, effect: Option<&(Effect, Vm)>) -> Lines {
    match auto {
        Auto::Skipped(_) => Lines::default(),
        Auto::Failed { path, error } => Lines {
            out: Vec::new(),
            warnings: vec![format!(
                "{path} was not changed: {error}; Docklet's light WSL VM settings are not applied"
            )],
        },
        Auto::Done { path, outcome } => {
            let mut lines = outcome_lines(path, outcome, verbose || outcome.changed, effect);
            if let Some(refused) = &outcome.refused {
                lines.out.clear();
                lines.warnings = vec![format!(
                    "{path} was left alone: {refused}. Docklet's light WSL VM settings are \
                     not applied; `docklet config set wslconfig keep` silences this"
                )];
            }
            lines
        }
    }
}

/// What `apply` or `revert` did, line by line. With `verbose`, keys that
/// did not change are named too.
pub fn outcome_lines(
    path: &str,
    outcome: &Outcome,
    verbose: bool,
    effect: Option<&(Effect, Vm)>,
) -> Lines {
    let mut lines = Lines::default();
    if let Some(refused) = &outcome.refused {
        lines
            .warnings
            .push(format!("{path} was left alone: {refused}"));
        return lines;
    }
    for key in &outcome.keys {
        if let Some(line) = key_line(key, verbose) {
            lines.out.push(line);
        }
    }
    if outcome.restored {
        lines.out.push(format!(
            ".wslconfig: put back as it was before Docklet changed it ({path})."
        ));
    } else if outcome.deleted {
        lines.out.push(format!(
            ".wslconfig: deleted {path}, which Docklet had created."
        ));
    }
    let added = outcome
        .keys
        .iter()
        .any(|key| matches!(key.result, KeyResult::Added | KeyResult::Updated));
    if added && outcome.changed {
        lines.out.push(format!(
            "Written to {path}. Undo: `{UNDO}`; to keep Docklet away from the file: \
             `docklet config set wslconfig keep`."
        ));
    }
    if let Some((effect, vm)) = effect.filter(|_| outcome.changed) {
        lines
            .out
            .push(wslconfig::effect_line(*effect, vm, DISTRO_NAME));
    }
    lines
}

fn key_line(key: &KeyReport, verbose: bool) -> Option<String> {
    let entry = &key.entry;
    let assignment = entry.assignment();
    let file_value = key.file_value.as_deref().unwrap_or("");
    let misplaced = match key.misplaced.as_slice() {
        [] => String::new(),
        lines => format!(
            " (line {} of the file sets it outside [{}], where WSL ignores it)",
            lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            entry.section
        ),
    };
    let line = match key.result {
        KeyResult::Added => format!(
            ".wslconfig: added {assignment} under [{}]: {}{misplaced}.",
            entry.section,
            entry.cost()
        ),
        KeyResult::Updated => format!(
            ".wslconfig: changed {} from {file_value} to {}: {}.",
            entry.key,
            entry.value,
            entry.cost()
        ),
        KeyResult::Removed => format!(".wslconfig: removed {assignment}, which Docklet had added."),
        KeyResult::Kept => format!(
            "{} is set to {file_value} in your .wslconfig; kept.",
            entry.key
        ),
        KeyResult::AlreadySet if verbose => {
            format!("{} is already {file_value} in your .wslconfig.", entry.key)
        }
        KeyResult::Unchanged if verbose => format!(".wslconfig: {assignment} is in place."),
        KeyResult::Released if key.file_value.is_some() => format!(
            "{} was changed to {file_value} after Docklet wrote it; it is yours now and stays.",
            entry.key
        ),
        KeyResult::Released if verbose => {
            format!("{assignment} was no longer in the file.")
        }
        _ => return None,
    };
    Some(line)
}

fn key_data(key: &KeyReport) -> Json {
    Object::new()
        .set("section", key.entry.section.as_str())
        .set("key", key.entry.key.as_str())
        .set("value", key.entry.value.as_str())
        .set("result", key.result.as_str())
        .set("file_value", key.file_value.clone())
        .into()
}

/// An outcome as `--json` reports it.
pub fn outcome_data(path: &str, outcome: &Outcome, effect: Option<Effect>) -> Object {
    let result = if outcome.refused.is_some() {
        "refused"
    } else if outcome.changed {
        "changed"
    } else {
        "unchanged"
    };
    Object::new()
        .set("path", path)
        .set("result", result)
        .set("reason", outcome.refused.clone())
        .set(
            "keys",
            outcome.keys.iter().map(key_data).collect::<Vec<_>>(),
        )
        .set("restored", outcome.restored)
        .set("deleted", outcome.deleted)
        .set("effect", effect.map(Effect::as_str))
        .set("undo", UNDO)
}

/// An automatic change as `--json` reports it, under `wslconfig`.
pub fn auto_data(auto: &Auto, effect: Option<Effect>) -> Json {
    match auto {
        Auto::Skipped(why) => Object::new()
            .set("result", "skipped")
            .set("reason", *why)
            .into(),
        Auto::Failed { path, error } => Object::new()
            .set("path", path.as_str())
            .set("result", "failed")
            .set("reason", error.as_str())
            .into(),
        Auto::Done { path, outcome } => outcome_data(path, outcome, effect).into(),
    }
}

/// Docklet's keys in the file now, what `apply` would still do, and
/// whether what is written is in effect. `vm` is asked only when Docklet
/// has written something.
pub fn status(settings: &Settings, vm: impl FnOnce() -> Option<Vm>) -> (Option<String>, Status) {
    let Ok(paths) = docklet_wsl::wslconfig_paths() else {
        return (
            None,
            Status {
                keep: settings.wslconfig == WslconfigMode::Keep,
                problem: Some("USERPROFILE or LOCALAPPDATA is not set".to_string()),
                ..Status::default()
            },
        );
    };
    let path = paths.file.display().to_string();
    let inspection = match wslconfig_file::inspect(&paths, &settings.wslconfig_entries()) {
        Ok(inspection) => inspection,
        Err(err) => {
            return (
                Some(path),
                Status {
                    keep: settings.wslconfig == WslconfigMode::Keep,
                    problem: Some(err.to_string()),
                    ..Status::default()
                },
            );
        }
    };
    let mut status = Status::from_keys(&inspection.keys);
    status.keep = settings.wslconfig == WslconfigMode::Keep;
    status.problem = inspection.refused;
    if !status.written.is_empty()
        && let (Some(modified), Some(vm)) = (inspection.modified, vm())
    {
        status.effect = Some(wslconfig::effect(&vm, modified));
        status.others = vm.others(DISTRO_NAME);
    }
    (Some(path), status)
}

/// The status as `status --json` and `wslconfig status --json` report it.
pub fn status_data(path: Option<&str>, settings: &Settings, status: &Status) -> Json {
    let assignments = |entries: &[wslconfig::Entry]| -> Vec<String> {
        entries.iter().map(wslconfig::Entry::assignment).collect()
    };
    let kept: Vec<Json> = status
        .kept
        .iter()
        .map(|(entry, value)| {
            Object::new()
                .set("key", entry.key.as_str())
                .set("value", value.as_str())
                .into()
        })
        .collect();
    Object::new()
        .set("path", path)
        .set("setting", settings.wslconfig.as_str())
        .set("vm_memory", settings.vm_memory.map(|size| size.to_string()))
        .set("written", assignments(&status.written))
        .set("kept", kept)
        .set("missing", assignments(&status.missing))
        .set("effect", status.effect.map(Effect::as_str))
        .set("pending", status.effect == Some(Effect::Pending))
        .set("others_running", status.others.clone())
        .set("problem", status.problem.clone())
        .into()
}

/// What `docklet wslconfig status` prints.
pub fn status_lines(path: Option<&str>, settings: &Settings, status: &Status) -> Vec<String> {
    let list = |entries: &[wslconfig::Entry]| {
        entries
            .iter()
            .map(wslconfig::Entry::assignment)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![
        format!("File: {}", path.unwrap_or("unknown")),
        format!(
            "Settings: wslconfig = {}, vm-memory = {}",
            settings.wslconfig,
            MemorySize::setting(settings.vm_memory)
        ),
    ];
    if status.written.is_empty() {
        lines.push("Written by Docklet: nothing".to_string());
    } else {
        lines.push(format!("Written by Docklet: {}", list(&status.written)));
    }
    for (entry, value) in &status.kept {
        lines.push(format!(
            "{} is set to {value} in your .wslconfig; kept.",
            entry.key
        ));
    }
    if !status.missing.is_empty() {
        lines.push(format!(
            "Not written yet: {} (`docklet wslconfig apply` writes it)",
            list(&status.missing)
        ));
    }
    if let Some(problem) = &status.problem {
        lines.push(format!("Docklet cannot change the file: {problem}."));
    }
    match status.effect {
        Some(Effect::InEffect) => {
            lines.push("In effect: the WSL VM started after the change.".to_string())
        }
        Some(Effect::Pending) => lines.push(format!(
            "Not yet in effect: the WSL VM started before the change. {}",
            restart_advice(&status.others)
        )),
        Some(Effect::Unknown) => lines.push(format!(
            "The WSL VM is running; whether it started before the change is unknown. If it \
             did: {}",
            restart_advice(&status.others)
        )),
        Some(Effect::NextStart) => {
            lines.push("WSL is not running; it reads the file when it starts.".to_string());
        }
        None => {}
    }
    lines
}

fn restart_advice(others: &[String]) -> String {
    if others.is_empty() {
        "Run `docklet stop`, wait a minute for the WSL VM to stop, then `docklet start`."
            .to_string()
    } else {
        format!(
            "Run `wsl --shutdown` once the other distros ({}) are idle.",
            others.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docklet_core::wslconfig::{Entry, light_entries};

    fn report(entry: Entry, result: KeyResult, file_value: Option<&str>) -> KeyReport {
        KeyReport {
            entry,
            result,
            file_value: file_value.map(str::to_string),
            misplaced: Vec::new(),
        }
    }

    fn gui() -> Entry {
        light_entries(None).remove(0)
    }

    const PATH: &str = r"C:\Users\me\.wslconfig";

    #[test]
    fn a_first_write_says_what_it_did_what_it_costs_and_how_to_undo_it() {
        let outcome = Outcome {
            keys: vec![report(gui(), KeyResult::Added, None)],
            changed: true,
            ..Outcome::default()
        };
        let vm = Vm::Running {
            distros: vec!["Ubuntu-24.04".to_string()],
            booted: None,
        };
        let lines = outcome_lines(PATH, &outcome, false, Some(&(Effect::Pending, vm)));
        assert_eq!(
            lines.out,
            [
                ".wslconfig: added guiApplications=false under [wsl2]: Linux GUI apps (WSLg) \
                 are off in every WSL distro.",
                r"Written to C:\Users\me\.wslconfig. Undo: `docklet wslconfig revert`; to keep Docklet away from the file: `docklet config set wslconfig keep`.",
                "It takes effect the next time WSL starts: WSL is running now (Ubuntu-24.04); \
                 run `wsl --shutdown` when those distros are idle.",
            ]
        );
        assert!(lines.warnings.is_empty());
    }

    #[test]
    fn a_key_of_the_users_is_named_and_kept() {
        let outcome = Outcome {
            keys: vec![report(gui(), KeyResult::Kept, Some("true"))],
            ..Outcome::default()
        };
        let lines = outcome_lines(PATH, &outcome, false, None);
        assert_eq!(
            lines.out,
            ["guiApplications is set to true in your .wslconfig; kept."]
        );
    }

    #[test]
    fn an_up_that_changes_nothing_says_nothing() {
        let auto = Auto::Done {
            path: PATH.to_string(),
            outcome: Outcome {
                keys: vec![report(gui(), KeyResult::Unchanged, None)],
                ..Outcome::default()
            },
        };
        assert_eq!(auto_lines(&auto, false, None), Lines::default());
        assert!(!auto.changed());
        // An install names what is in place.
        assert_eq!(
            auto_lines(&auto, true, None).out,
            [".wslconfig: guiApplications=false is in place."]
        );
        assert_eq!(
            auto_lines(&Auto::Skipped("flag"), true, None),
            Lines::default()
        );
    }

    #[test]
    fn a_file_docklet_may_not_touch_is_a_warning() {
        let auto = Auto::Done {
            path: PATH.to_string(),
            outcome: Outcome {
                refused: Some("it is read-only".to_string()),
                ..Outcome::default()
            },
        };
        let lines = auto_lines(&auto, true, None);
        assert!(lines.out.is_empty());
        assert_eq!(
            lines.warnings,
            [
                r"C:\Users\me\.wslconfig was left alone: it is read-only. Docklet's light WSL VM settings are not applied; `docklet config set wslconfig keep` silences this"
            ]
        );
        let failed = Auto::Failed {
            path: PATH.to_string(),
            error: "access denied".to_string(),
        };
        assert!(auto_lines(&failed, false, None).warnings[0].contains("access denied"));
    }

    #[test]
    fn a_revert_says_the_file_is_back() {
        let outcome = Outcome {
            keys: vec![report(gui(), KeyResult::Removed, None)],
            changed: true,
            restored: true,
            ..Outcome::default()
        };
        let lines = outcome_lines(
            PATH,
            &outcome,
            false,
            Some(&(Effect::NextStart, Vm::Stopped)),
        );
        assert_eq!(
            lines.out,
            [
                ".wslconfig: removed guiApplications=false, which Docklet had added.",
                r".wslconfig: put back as it was before Docklet changed it (C:\Users\me\.wslconfig).",
                "It takes effect the next time WSL starts; no WSL distro is running now.",
            ]
        );
    }

    #[test]
    fn the_data_of_an_outcome() {
        let outcome = Outcome {
            keys: vec![report(gui(), KeyResult::Added, None)],
            changed: true,
            ..Outcome::default()
        };
        let data: Json = outcome_data("p", &outcome, Some(Effect::InEffect)).into();
        assert_eq!(
            data.render(),
            concat!(
                r#"{"path":"p","result":"changed","reason":null,"keys":[{"section":"wsl2","#,
                r#""key":"guiApplications","value":"false","result":"added","file_value":null}],"#,
                r#""restored":false,"deleted":false,"effect":"in_effect","#,
                r#""undo":"docklet wslconfig revert"}"#
            )
        );
        assert_eq!(
            auto_data(&Auto::Skipped("setting"), None).render(),
            r#"{"result":"skipped","reason":"setting"}"#
        );
    }

    #[test]
    fn the_status_data_says_when_light_settings_are_not_in_effect_yet() {
        let status = Status {
            written: light_entries(None),
            effect: Some(Effect::Pending),
            others: vec!["Ubuntu".to_string()],
            ..Status::default()
        };
        let settings = Settings::default();
        assert_eq!(
            status_data(Some("p"), &settings, &status).render(),
            concat!(
                r#"{"path":"p","setting":"light","vm_memory":null,"#,
                r#""written":["guiApplications=false"],"kept":[],"missing":[],"#,
                r#""effect":"pending","pending":true,"others_running":["Ubuntu"],"problem":null}"#
            )
        );
        let lines = status_lines(Some("p"), &settings, &status);
        assert_eq!(
            lines,
            [
                "File: p",
                "Settings: wslconfig = light, vm-memory = off",
                "Written by Docklet: guiApplications=false",
                "Not yet in effect: the WSL VM started before the change. Run `wsl --shutdown` \
                 once the other distros (Ubuntu) are idle.",
            ]
        );
    }
}
