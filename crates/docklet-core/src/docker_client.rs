//! Finding the Windows Docker client pieces on disk: `docker.exe`, the
//! Compose plugin and a Docker context.
//!
//! Everything here only reads. No process is started, so a status query or a
//! diagnosis can use it without a deadline.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const DOCKER_EXE: &str = "docker.exe";

/// A `docker.exe` and how it has to be called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerCli {
    pub path: PathBuf,
    /// False for the copy Docklet installed, which is not on PATH.
    pub on_path: bool,
}

/// The `docker.exe` Docklet uses: the first one on PATH, otherwise the copy
/// `docklet cli setup` puts under `data_dir`. That is the order `cli setup`
/// itself follows, so both always name the same file.
///
/// Relative PATH entries (`.`, empty) are skipped: they resolve against the
/// current directory, which may be untrusted.
pub fn find_docker(path_var: Option<&OsStr>, data_dir: &Path) -> Option<DockerCli> {
    let on_path = path_var.and_then(|path| {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(DOCKER_EXE))
            .find(|candidate| candidate.is_file())
    });
    if let Some(path) = on_path {
        return Some(DockerCli {
            path,
            on_path: true,
        });
    }
    let own = data_dir.join("bin").join(DOCKER_EXE);
    own.is_file().then_some(DockerCli {
        path: own,
        on_path: false,
    })
}

/// Where `docklet cli setup` installs the Compose plugin.
pub fn compose_plugin(home: &Path) -> PathBuf {
    home.join(".docker")
        .join("cli-plugins")
        .join("docker-compose.exe")
}

/// The Docker CLI's configuration directory: `DOCKER_CONFIG` when set,
/// otherwise `<home>\.docker`.
pub fn config_dir(docker_config: Option<&OsStr>, home: &Path) -> PathBuf {
    match docker_config.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".docker"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextState {
    Missing,
    /// Exists and points at the expected Docker host.
    Present,
    /// A context of that name exists but points somewhere else.
    OtherHost,
}

/// A user with more contexts than this has something unusual going on; the
/// scan stops rather than read without bound.
const MAX_CONTEXTS: usize = 512;
/// A `meta.json` is a few hundred bytes.
const MAX_META_BYTES: u64 = 64 * 1024;

/// Looks for the context `name` in the Docker CLI's context store.
///
/// The store keeps one `contexts/meta/<digest of the name>/meta.json` per
/// context. The files are read rather than the digest computed, which keeps
/// a hash out of this crate and survives a change of the digest.
pub fn context_state(config_dir: &Path, name: &str, docker_host: &str) -> ContextState {
    let Ok(entries) = fs::read_dir(config_dir.join("contexts").join("meta")) else {
        return ContextState::Missing;
    };
    for entry in entries.take(MAX_CONTEXTS).flatten() {
        let meta = entry.path().join("meta.json");
        let small = fs::metadata(&meta).is_ok_and(|file| file.len() <= MAX_META_BYTES);
        if !small {
            continue;
        }
        let Ok(text) = fs::read_to_string(&meta) else {
            continue;
        };
        if let Some(state) = meta_state(&text, name, docker_host) {
            return state;
        }
    }
    ContextState::Missing
}

/// Judges one `meta.json`: `None` when it describes another context.
///
/// The file is JSON such as
/// `{"Name":"docklet","Metadata":{},"Endpoints":{"docker":{"Host":"npipe:////./pipe/docklet_engine"}}}`.
/// It is matched as text with the whitespace removed, which is exact for
/// what the Docker CLI writes and keeps a JSON parser out of the CLI. A
/// quote inside a string value is escaped in JSON, so a description cannot
/// pose as the name.
fn meta_state(meta: &str, name: &str, docker_host: &str) -> Option<ContextState> {
    let compact: String = meta.chars().filter(|c| !c.is_whitespace()).collect();
    if !compact.contains(&format!("\"Name\":\"{name}\"")) {
        return None;
    }
    if compact.contains(&format!("\"Host\":\"{docker_host}\"")) {
        Some(ContextState::Present)
    } else {
        Some(ContextState::OtherHost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ENGINE_DOCKER_HOST as HOST;
    use std::ffi::OsString;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "docklet-client-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const DOCKLET_META: &str = r#"{"Name":"docklet","Metadata":{"Description":"Docklet engine"},"Endpoints":{"docker":{"Host":"npipe:////./pipe/docklet_engine","SkipTLSVerify":false}}}"#;

    #[test]
    fn docker_on_path_wins_over_the_installed_copy() {
        let temp = TempDir::new("path-wins");
        let on_path = temp.file("tools/docker.exe", "");
        temp.file("data/bin/docker.exe", "");
        let path = std::env::join_paths([temp.0.join("empty"), temp.0.join("tools")]).unwrap();
        let found = find_docker(Some(&path), &temp.0.join("data")).unwrap();
        assert_eq!(found.path, on_path);
        assert!(found.on_path);
    }

    #[test]
    fn the_installed_copy_is_used_when_path_has_none() {
        let temp = TempDir::new("own-copy");
        let own = temp.file("data/bin/docker.exe", "");
        let path = std::env::join_paths([temp.0.join("empty")]).unwrap();
        let found = find_docker(Some(&path), &temp.0.join("data")).unwrap();
        assert_eq!(found.path, own);
        assert!(!found.on_path);
        assert_eq!(find_docker(None, &temp.0.join("data")).unwrap().path, own);
    }

    #[test]
    fn no_docker_anywhere() {
        let temp = TempDir::new("none");
        let path = std::env::join_paths([temp.0.join("empty")]).unwrap();
        assert_eq!(find_docker(Some(&path), &temp.0.join("data")), None);
        assert_eq!(find_docker(None, &temp.0.join("data")), None);
    }

    #[test]
    fn relative_path_entries_are_never_searched() {
        let temp = TempDir::new("relative");
        // `.` and an empty entry both mean the current directory.
        let path = std::env::join_paths([PathBuf::from("."), PathBuf::new()]).unwrap();
        assert_eq!(find_docker(Some(&path), &temp.0.join("data")), None);
    }

    #[test]
    fn a_directory_named_docker_exe_is_not_a_docker() {
        let temp = TempDir::new("directory");
        fs::create_dir_all(temp.0.join("tools").join("docker.exe")).unwrap();
        let path = std::env::join_paths([temp.0.join("tools")]).unwrap();
        assert_eq!(find_docker(Some(&path), &temp.0.join("data")), None);
    }

    #[test]
    fn compose_plugin_is_where_cli_setup_puts_it() {
        let path = compose_plugin(Path::new("home"));
        assert!(path.ends_with(Path::new(".docker/cli-plugins/docker-compose.exe")));
    }

    #[test]
    fn config_dir_follows_docker_config() {
        let home = Path::new("home");
        assert_eq!(config_dir(None, home), home.join(".docker"));
        let empty = OsString::new();
        assert_eq!(config_dir(Some(&empty), home), home.join(".docker"));
        let custom = OsString::from("elsewhere");
        assert_eq!(config_dir(Some(&custom), home), PathBuf::from("elsewhere"));
    }

    #[test]
    fn meta_of_the_docklet_context() {
        assert_eq!(
            meta_state(DOCKLET_META, "docklet", HOST),
            Some(ContextState::Present)
        );
    }

    #[test]
    fn meta_with_whitespace_still_matches() {
        let pretty = "{\r\n  \"Name\": \"docklet\",\r\n  \"Endpoints\": {\r\n    \"docker\": { \"Host\": \"npipe:////./pipe/docklet_engine\" }\r\n  }\r\n}\r\n";
        assert_eq!(
            meta_state(pretty, "docklet", HOST),
            Some(ContextState::Present)
        );
    }

    #[test]
    fn meta_of_another_context_is_not_a_match() {
        let other = r#"{"Name":"desktop-linux","Metadata":{},"Endpoints":{"docker":{"Host":"npipe:////./pipe/dockerDesktopLinuxEngine"}}}"#;
        assert_eq!(meta_state(other, "docklet", HOST), None);
        // A longer name that starts the same way is another context.
        let longer = DOCKLET_META.replace("\"docklet\"", "\"docklet-test\"");
        assert_eq!(meta_state(&longer, "docklet", HOST), None);
        assert_eq!(meta_state("", "docklet", HOST), None);
        assert_eq!(meta_state("not json", "docklet", HOST), None);
    }

    #[test]
    fn a_description_cannot_pose_as_the_name() {
        // What the Docker CLI writes for
        // `--description '"Name":"docklet"'` on a context called `other`.
        let sneaky = r#"{"Name":"other","Metadata":{"Description":"\"Name\":\"docklet\""},"Endpoints":{"docker":{"Host":"tcp://example:2375"}}}"#;
        assert_eq!(meta_state(sneaky, "docklet", HOST), None);
    }

    #[test]
    fn a_docklet_context_for_another_host_is_reported_as_such() {
        let elsewhere = DOCKLET_META.replace("docklet_engine", "something_else");
        assert_eq!(
            meta_state(&elsewhere, "docklet", HOST),
            Some(ContextState::OtherHost)
        );
    }

    #[test]
    fn finds_the_context_in_a_store_with_several() {
        let temp = TempDir::new("store");
        temp.file(
            "contexts/meta/aaaa/meta.json",
            r#"{"Name":"desktop-linux","Endpoints":{"docker":{"Host":"npipe:////./pipe/x"}}}"#,
        );
        temp.file("contexts/meta/bbbb/meta.json", DOCKLET_META);
        // Things that are not contexts are passed over.
        temp.file("contexts/meta/stray.txt", "hello");
        temp.file("contexts/meta/cccc/other.json", DOCKLET_META);
        assert_eq!(
            context_state(&temp.0, "docklet", HOST),
            ContextState::Present
        );
        assert_eq!(
            context_state(&temp.0, "missing", HOST),
            ContextState::Missing
        );
    }

    #[test]
    fn a_missing_store_means_no_context() {
        let temp = TempDir::new("no-store");
        assert_eq!(
            context_state(&temp.0, "docklet", HOST),
            ContextState::Missing
        );
    }

    #[test]
    fn an_oversized_meta_file_is_not_read() {
        let temp = TempDir::new("oversized");
        let padding = " ".repeat(MAX_META_BYTES as usize);
        temp.file(
            "contexts/meta/aaaa/meta.json",
            &format!("{DOCKLET_META}{padding}"),
        );
        assert_eq!(
            context_state(&temp.0, "docklet", HOST),
            ContextState::Missing
        );
    }
}
