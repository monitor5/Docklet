//! Registration of the Docklet agent skill.
//!
//! AI coding agents discover skills as `<skills dir>/<name>/SKILL.md`. Docklet
//! ships one skill that teaches an agent to drive the `docklet` CLI, and
//! registers it by writing that file into each known agent's skills directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The skill text, embedded so the CLI and the GUI need no extra files.
pub const SKILL_MD: &str = include_str!("../../../skills/docklet/SKILL.md");

const SKILL_NAME: &str = "docklet";
const SKILL_FILE: &str = "SKILL.md";

/// An agent whose user-level skills directory Docklet knows how to find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTarget {
    pub agent: &'static str,
    /// Agent config directory, e.g. `~/.claude`. Registration is skipped when
    /// it does not exist, so Docklet never creates config for absent agents.
    pub config_dir: PathBuf,
    /// Directory that holds one subdirectory per skill.
    pub skills_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillStatus {
    /// The agent's config directory does not exist.
    AgentNotFound,
    NotInstalled,
    /// Installed, but the content differs from this version's skill.
    Outdated,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    AgentNotFound,
    Installed,
    Updated,
    AlreadyCurrent,
}

/// Known agents, resolved against the user's home directory.
pub fn default_targets(home: &Path) -> Vec<AgentTarget> {
    [("Claude Code", ".claude"), ("Codex", ".codex")]
        .into_iter()
        .map(|(agent, dir)| AgentTarget {
            agent,
            config_dir: home.join(dir),
            skills_dir: home.join(dir).join("skills"),
        })
        .collect()
}

impl AgentTarget {
    pub fn skill_path(&self) -> PathBuf {
        self.skills_dir.join(SKILL_NAME).join(SKILL_FILE)
    }

    pub fn status(&self) -> SkillStatus {
        if !self.config_dir.is_dir() {
            return SkillStatus::AgentNotFound;
        }
        match fs::read_to_string(self.skill_path()) {
            Ok(existing) if normalize(&existing) == normalize(SKILL_MD) => SkillStatus::Current,
            Ok(_) => SkillStatus::Outdated,
            Err(_) => SkillStatus::NotInstalled,
        }
    }

    /// Writes the skill. Idempotent: an up-to-date file is left untouched.
    pub fn install(&self) -> io::Result<InstallOutcome> {
        let outcome = match self.status() {
            SkillStatus::AgentNotFound => return Ok(InstallOutcome::AgentNotFound),
            SkillStatus::Current => return Ok(InstallOutcome::AlreadyCurrent),
            SkillStatus::Outdated => InstallOutcome::Updated,
            SkillStatus::NotInstalled => InstallOutcome::Installed,
        };
        let path = self.skill_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, SKILL_MD)?;
        Ok(outcome)
    }

    /// Removes the skill directory. Returns whether anything was removed.
    ///
    /// Only `SKILL.md` and its then-empty directory are removed, so files a
    /// user added next to it survive.
    pub fn uninstall(&self) -> io::Result<bool> {
        let path = self.skill_path();
        if !path.is_file() {
            return Ok(false);
        }
        fs::remove_file(&path)?;
        if let Some(dir) = path.parent() {
            // Fails when the directory is not empty, which is intended.
            let _ = fs::remove_dir(dir);
        }
        Ok(true)
    }
}

/// Git may check the skill out with CRLF line endings on Windows.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "docklet-skills-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn claude(&self) -> AgentTarget {
            default_targets(&self.0).remove(0)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn skill_has_frontmatter_with_name() {
        assert!(normalize(SKILL_MD).starts_with("---\nname: docklet\n"));
    }

    #[test]
    fn skips_agents_that_are_not_present() {
        let home = TempHome::new("absent");
        let target = home.claude();
        assert_eq!(target.status(), SkillStatus::AgentNotFound);
        assert_eq!(target.install().unwrap(), InstallOutcome::AgentNotFound);
        assert!(!target.config_dir.exists());
    }

    #[test]
    fn install_update_uninstall_roundtrip() {
        let home = TempHome::new("roundtrip");
        let target = home.claude();
        fs::create_dir_all(&target.config_dir).unwrap();

        assert_eq!(target.status(), SkillStatus::NotInstalled);
        assert_eq!(target.install().unwrap(), InstallOutcome::Installed);
        assert_eq!(target.status(), SkillStatus::Current);
        assert_eq!(target.install().unwrap(), InstallOutcome::AlreadyCurrent);

        fs::write(target.skill_path(), "stale").unwrap();
        assert_eq!(target.status(), SkillStatus::Outdated);
        assert_eq!(target.install().unwrap(), InstallOutcome::Updated);
        assert_eq!(fs::read_to_string(target.skill_path()).unwrap(), SKILL_MD);

        assert!(target.uninstall().unwrap());
        assert!(!target.uninstall().unwrap());
        assert_eq!(target.status(), SkillStatus::NotInstalled);
        assert!(!target.skills_dir.join("docklet").exists());
    }

    #[test]
    fn uninstall_keeps_user_files() {
        let home = TempHome::new("keep");
        let target = home.claude();
        fs::create_dir_all(&target.config_dir).unwrap();
        target.install().unwrap();
        let extra = target.skills_dir.join("docklet").join("notes.md");
        fs::write(&extra, "mine").unwrap();

        assert!(target.uninstall().unwrap());
        assert!(extra.is_file());
    }
}
