//! Failures and the error codes a program can branch on.
//!
//! The codes are part of the CLI's interface: they appear in `--json` output,
//! in `docklet help --json` and in the agent skill. Renaming one breaks
//! callers; adding one means adding it to [`ErrorCode::ALL`], which the tests
//! tie to the skill.

use docklet_core::doctor::{EngineFacts, WslFact};
use docklet_core::engine;
use docklet_core::wsl::WslVersion;

use crate::catalog;
use crate::json::{Json, Object};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Usage,
    ConfirmationRequired,
    WslMissing,
    WslOutdated,
    WslError,
    NotInstalled,
    AlreadyInstalled,
    UnknownBase,
    NotRunning,
    Timeout,
    DockerCliMissing,
    ChecksFailed,
    Failed,
}

impl ErrorCode {
    pub const ALL: [Self; 13] = [
        Self::Usage,
        Self::ConfirmationRequired,
        Self::WslMissing,
        Self::WslOutdated,
        Self::WslError,
        Self::NotInstalled,
        Self::AlreadyInstalled,
        Self::UnknownBase,
        Self::NotRunning,
        Self::Timeout,
        Self::DockerCliMissing,
        Self::ChecksFailed,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::ConfirmationRequired => "confirmation_required",
            Self::WslMissing => "wsl_missing",
            Self::WslOutdated => "wsl_outdated",
            Self::WslError => "wsl_error",
            Self::NotInstalled => "not_installed",
            Self::AlreadyInstalled => "already_installed",
            Self::UnknownBase => "unknown_base",
            Self::NotRunning => "not_running",
            Self::Timeout => "timeout",
            Self::DockerCliMissing => "docker_cli_missing",
            Self::ChecksFailed => "checks_failed",
            Self::Failed => "failed",
        }
    }

    /// One line for `docklet help --json`.
    pub fn meaning(self) -> &'static str {
        match self {
            Self::Usage => "The command line is wrong",
            Self::ConfirmationRequired => {
                "The command deletes data and was run without --yes; nothing was deleted"
            }
            Self::WslMissing => "WSL is not installed",
            Self::WslOutdated => "WSL is older than the oldest version Docklet supports",
            Self::WslError => "wsl.exe failed or did not answer in time",
            Self::NotInstalled => "The docklet distro is not installed",
            Self::AlreadyInstalled => "The docklet distro is already installed",
            Self::UnknownBase => "The name given to --base is not a published base",
            Self::NotRunning => "The engine or its pipe proxy is not running",
            Self::Timeout => "The engine did not become usable in time",
            Self::DockerCliMissing => "No docker.exe was found",
            Self::ChecksFailed => "`docklet doctor` found a failing check; they are in `data`",
            Self::Failed => "Anything else; `message` says what went wrong",
        }
    }

    /// 2 when the command line has to change, 1 for everything else.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Usage | Self::ConfirmationRequired => 2,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
    /// The next command to run, when there is an obvious one.
    pub hint: Option<&'static str>,
    /// What the command found out before it failed (`doctor`, `skills`).
    pub data: Option<Json>,
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            data: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Usage, message).hint("docklet help")
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Failed, message)
    }

    pub fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }

    pub fn with_data(mut self, data: Json) -> Self {
        self.data = Some(data);
        self
    }

    /// What goes to stderr without `--json`.
    ///
    /// A wrong command line gets the help text after it. A hint is added to
    /// the message; failures of the commands that predate `--json` carry
    /// none in text mode, so their text is what it always was.
    pub fn text(&self) -> String {
        if self.code.exit_code() == 2 {
            return format!("docklet: {}\n\n{}", self.message, catalog::help_text());
        }
        match self.hint {
            Some(hint) => format!("docklet: {}; run `{hint}`\n", self.message),
            None => format!("docklet: {}\n", self.message),
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(err: std::io::Error) -> Self {
        Self::failed(err.to_string())
    }
}

/// The one object on stdout for a command that succeeded.
pub fn success_envelope(command: Option<&str>, data: Json) -> Json {
    Object::new()
        .set("ok", true)
        .set("command", command)
        .set("data", data)
        .into()
}

/// The one object on stdout for a command that failed.
pub fn failure_envelope(command: Option<&str>, failure: &Failure) -> Json {
    let error = Object::new()
        .set("code", failure.code.as_str())
        .set("message", failure.message.as_str())
        .set_some("hint", failure.hint);
    Object::new()
        .set("ok", false)
        .set("command", command)
        .set("error", error)
        .set_some("data", failure.data.clone())
        .into()
}

/// Why the engine cannot be used right now, or `None` when it can.
///
/// This is the one definition of "usable" that `docker`, `wait` and `up`
/// share: WSL works, the distro is installed and running, dockerd answers and
/// the pipe proxy is up.
pub fn blocker(facts: &EngineFacts, proxy_running: bool) -> Option<Failure> {
    let wsl_error =
        |reason: &String| Failure::new(ErrorCode::WslError, reason.clone()).hint("docklet doctor");
    let not_running =
        |message: &str| Failure::new(ErrorCode::NotRunning, message).hint("docklet start");
    match &facts.wsl {
        WslFact::Missing => {
            return Some(
                Failure::new(
                    ErrorCode::WslMissing,
                    "WSL is not installed (installing it needs an administrator and a reboot)",
                )
                .hint("wsl --install --no-distribution"),
            );
        }
        WslFact::Unresponsive(reason) => return Some(wsl_error(reason)),
        WslFact::Found(version) if *version < WslVersion::MINIMUM => {
            let message = format!(
                "WSL {version} is too old; Docklet needs {} or later",
                WslVersion::MINIMUM
            );
            return Some(Failure::new(ErrorCode::WslOutdated, message).hint("wsl --update"));
        }
        WslFact::Found(_) => {}
    }
    match &facts.installed {
        Some(Ok(true)) => {}
        Some(Err(reason)) => return Some(wsl_error(reason)),
        _ => {
            return Some(
                Failure::new(
                    ErrorCode::NotInstalled,
                    "the docklet distro is not installed",
                )
                .hint("docklet install"),
            );
        }
    }
    match &facts.running {
        Some(Ok(true)) => {}
        Some(Err(reason)) => return Some(wsl_error(reason)),
        _ => return Some(not_running("the engine is stopped")),
    }
    if facts.dockerd != Some(true) {
        return Some(not_running("the distro is up, Docker is not ready yet"));
    }
    if !proxy_running {
        return Some(not_running("Docker is up but the pipe proxy is not"));
    }
    None
}

/// The engine operation that failed, for [`classify`].
#[derive(Debug, Clone, Copy)]
pub enum Op<'a> {
    /// `base` is the name given to `--base`, if one was.
    Install {
        base: Option<&'a str>,
    },
    Start,
    Other,
}

/// Gives a failed engine operation its error code.
///
/// The operations in `docklet-wsl` report failures as text. Rather than
/// match on that text, the engine is looked at again after the failure and
/// the code follows from what is found. `message` stays the operation's own.
pub fn classify(op: Op, message: String, facts: &EngineFacts) -> Failure {
    let cause = blocker(facts, true);
    let cause_code = cause.as_ref().map(|cause| cause.code);
    let cause_hint = cause.and_then(|cause| cause.hint);
    let (code, hint) = match (op, cause_code) {
        // WSL itself is the problem, whatever was attempted.
        (
            _,
            Some(code @ (ErrorCode::WslMissing | ErrorCode::WslOutdated | ErrorCode::WslError)),
        ) => (code, cause_hint),
        // An install only fails on an installed distro because it is there.
        (Op::Install { .. }, None | Some(ErrorCode::NotRunning)) => {
            (ErrorCode::AlreadyInstalled, Some("docklet start"))
        }
        (Op::Install { base: Some(base) }, _) if !engine::bases().contains(&base) => {
            (ErrorCode::UnknownBase, Some("docklet bases"))
        }
        (Op::Install { .. }, _) => (ErrorCode::Failed, None),
        (Op::Start | Op::Other, Some(ErrorCode::NotInstalled)) => {
            (ErrorCode::NotInstalled, cause_hint)
        }
        // The distro came up and dockerd never answered.
        (Op::Start, Some(ErrorCode::NotRunning)) if facts.dockerd == Some(false) => {
            (ErrorCode::Timeout, Some("docklet doctor"))
        }
        (Op::Start, _) => (ErrorCode::Failed, Some("docklet doctor")),
        (Op::Other, _) => (ErrorCode::Failed, None),
    };
    Failure {
        code,
        message,
        hint,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docklet_core::skills::SKILL_MD;

    fn facts(
        wsl: WslFact,
        installed: Option<Result<bool, String>>,
        running: Option<Result<bool, String>>,
        dockerd: Option<bool>,
    ) -> EngineFacts {
        EngineFacts {
            wsl,
            installed,
            running,
            dockerd,
        }
    }

    fn current() -> WslFact {
        WslFact::Found(WslVersion {
            major: 3,
            minor: 0,
            patch: 5,
        })
    }

    fn missing() -> EngineFacts {
        facts(WslFact::Missing, None, None, None)
    }

    fn outdated() -> EngineFacts {
        let old = WslVersion {
            major: 2,
            minor: 3,
            patch: 26,
        };
        facts(WslFact::Found(old), None, None, None)
    }

    fn hung() -> EngineFacts {
        let reason = "`wsl --version` did not finish within 30 s and was stopped";
        facts(WslFact::Unresponsive(reason.to_string()), None, None, None)
    }

    fn not_installed() -> EngineFacts {
        facts(current(), Some(Ok(false)), None, None)
    }

    fn stopped() -> EngineFacts {
        facts(current(), Some(Ok(true)), Some(Ok(false)), None)
    }

    fn starting() -> EngineFacts {
        facts(current(), Some(Ok(true)), Some(Ok(true)), Some(false))
    }

    fn running() -> EngineFacts {
        facts(current(), Some(Ok(true)), Some(Ok(true)), Some(true))
    }

    #[test]
    fn codes_are_unique_snake_case_and_all_listed() {
        let names: Vec<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
        for name in &names {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}"
            );
            assert_eq!(names.iter().filter(|other| other == &name).count(), 1);
        }
        // Fails to compile when a variant is added without a decision here,
        // which is the reminder to add it to `ALL` as well.
        for code in ErrorCode::ALL {
            match code {
                ErrorCode::Usage
                | ErrorCode::ConfirmationRequired
                | ErrorCode::WslMissing
                | ErrorCode::WslOutdated
                | ErrorCode::WslError
                | ErrorCode::NotInstalled
                | ErrorCode::AlreadyInstalled
                | ErrorCode::UnknownBase
                | ErrorCode::NotRunning
                | ErrorCode::Timeout
                | ErrorCode::DockerCliMissing
                | ErrorCode::ChecksFailed
                | ErrorCode::Failed => {}
            }
        }
    }

    #[test]
    fn the_skill_lists_every_error_code() {
        for code in ErrorCode::ALL {
            assert!(
                SKILL_MD.contains(&format!("`{}`", code.as_str())),
                "skills/docklet/SKILL.md does not mention `{}`",
                code.as_str()
            );
        }
    }

    #[test]
    fn only_a_wrong_command_line_exits_with_2() {
        for code in ErrorCode::ALL {
            let expected = match code {
                ErrorCode::Usage | ErrorCode::ConfirmationRequired => 2,
                _ => 1,
            };
            assert_eq!(code.exit_code(), expected, "{code:?}");
        }
    }

    #[test]
    fn success_envelope_shape() {
        let data = Object::new().set("state", "running").into();
        assert_eq!(
            success_envelope(Some("status"), data).render(),
            r#"{"ok":true,"command":"status","data":{"state":"running"}}"#
        );
    }

    #[test]
    fn failure_envelope_shape() {
        let failure = Failure::new(
            ErrorCode::NotInstalled,
            "the docklet distro is not installed",
        )
        .hint("docklet install");
        assert_eq!(
            failure_envelope(Some("start"), &failure).render(),
            r#"{"ok":false,"command":"start","error":{"code":"not_installed","message":"the docklet distro is not installed","hint":"docklet install"}}"#
        );
    }

    #[test]
    fn a_failure_without_a_hint_has_no_hint_field() {
        let failure = Failure::failed("disk full");
        assert_eq!(
            failure_envelope(Some("install"), &failure).render(),
            r#"{"ok":false,"command":"install","error":{"code":"failed","message":"disk full"}}"#
        );
    }

    #[test]
    fn an_unknown_command_has_a_null_command() {
        let failure = Failure::usage("unknown command `frobnicate`");
        assert_eq!(
            failure_envelope(None, &failure).render(),
            r#"{"ok":false,"command":null,"error":{"code":"usage","message":"unknown command `frobnicate`","hint":"docklet help"}}"#
        );
    }

    #[test]
    fn a_failure_can_carry_what_was_found() {
        let data = Object::new().set("healthy", false).into();
        let failure = Failure::new(ErrorCode::ChecksFailed, "1 check failed").with_data(data);
        assert_eq!(
            failure_envelope(Some("doctor"), &failure).render(),
            r#"{"ok":false,"command":"doctor","error":{"code":"checks_failed","message":"1 check failed"},"data":{"healthy":false}}"#
        );
    }

    #[test]
    fn text_of_a_plain_failure_is_one_line() {
        assert_eq!(
            Failure::failed("the pipe proxy did not exit").text(),
            "docklet: the pipe proxy did not exit\n"
        );
        let from_io = Failure::from(std::io::Error::other("WSL is not installed"));
        assert_eq!(from_io.code, ErrorCode::Failed);
        assert_eq!(from_io.text(), "docklet: WSL is not installed\n");
    }

    #[test]
    fn text_of_a_failure_with_a_hint_names_the_command() {
        let failure =
            Failure::new(ErrorCode::NotRunning, "the engine is stopped").hint("docklet start");
        assert_eq!(
            failure.text(),
            "docklet: the engine is stopped; run `docklet start`\n"
        );
    }

    #[test]
    fn text_of_a_wrong_command_line_is_followed_by_the_help() {
        let text = Failure::usage("unknown command `frobnicate`").text();
        let expected = format!(
            "docklet: unknown command `frobnicate`\n\n{}",
            catalog::help_text()
        );
        assert_eq!(text, expected);
        let confirm =
            Failure::new(ErrorCode::ConfirmationRequired, "pass --yes").hint("docklet x --yes");
        assert!(
            confirm
                .text()
                .starts_with("docklet: pass --yes\n\nDocklet - ")
        );
    }

    #[test]
    fn nothing_blocks_a_running_engine_with_its_proxy() {
        assert_eq!(blocker(&running(), true), None);
    }

    #[test]
    fn blockers_in_the_order_they_have_to_be_fixed() {
        let cases = [
            (
                missing(),
                ErrorCode::WslMissing,
                "wsl --install --no-distribution",
            ),
            (outdated(), ErrorCode::WslOutdated, "wsl --update"),
            (hung(), ErrorCode::WslError, "docklet doctor"),
            (not_installed(), ErrorCode::NotInstalled, "docklet install"),
            (stopped(), ErrorCode::NotRunning, "docklet start"),
            (starting(), ErrorCode::NotRunning, "docklet start"),
        ];
        for (facts, code, hint) in cases {
            // Whether the proxy is up does not matter until Docker is.
            for proxy in [true, false] {
                let failure = blocker(&facts, proxy).unwrap();
                assert_eq!(failure.code, code, "{facts:?}");
                assert_eq!(failure.hint, Some(hint), "{facts:?}");
                assert!(!failure.message.is_empty());
            }
        }
    }

    #[test]
    fn a_missing_proxy_blocks_an_engine_that_is_otherwise_up() {
        let failure = blocker(&running(), false).unwrap();
        assert_eq!(failure.code, ErrorCode::NotRunning);
        assert_eq!(failure.hint, Some("docklet start"));
        assert!(failure.message.contains("pipe proxy"));
    }

    #[test]
    fn blocker_messages_carry_the_details() {
        assert!(
            blocker(&outdated(), true)
                .unwrap()
                .message
                .contains("2.3.26")
        );
        assert!(
            blocker(&outdated(), true)
                .unwrap()
                .message
                .contains("2.4.4")
        );
        assert!(
            blocker(&hung(), true)
                .unwrap()
                .message
                .contains("did not finish")
        );
    }

    #[test]
    fn a_list_that_fails_is_a_wsl_error() {
        let list_failed = Some(Err("`wsl --list --quiet` did not finish".to_string()));
        let installed_unknown = facts(current(), list_failed.clone(), None, None);
        let failure = blocker(&installed_unknown, true).unwrap();
        assert_eq!(failure.code, ErrorCode::WslError);
        assert!(failure.message.contains("--list"));
        let running_unknown = facts(current(), Some(Ok(true)), list_failed, None);
        assert_eq!(
            blocker(&running_unknown, true).unwrap().code,
            ErrorCode::WslError
        );
    }

    fn classified(op: Op, facts: &EngineFacts) -> (ErrorCode, Option<&'static str>) {
        let failure = classify(op, "the operation's own words".to_string(), facts);
        assert_eq!(failure.message, "the operation's own words");
        assert_eq!(failure.data, None);
        (failure.code, failure.hint)
    }

    #[test]
    fn a_wsl_problem_wins_whatever_was_attempted() {
        let ops = [
            Op::Install { base: None },
            Op::Install { base: Some("nope") },
            Op::Start,
            Op::Other,
        ];
        for op in ops {
            assert_eq!(
                classified(op, &missing()),
                (
                    ErrorCode::WslMissing,
                    Some("wsl --install --no-distribution")
                )
            );
            assert_eq!(
                classified(op, &outdated()),
                (ErrorCode::WslOutdated, Some("wsl --update"))
            );
            assert_eq!(
                classified(op, &hung()),
                (ErrorCode::WslError, Some("docklet doctor"))
            );
        }
    }

    #[test]
    fn a_failed_install_on_an_installed_distro_is_already_installed() {
        for facts in [stopped(), starting(), running()] {
            for base in [None, Some("debian-13"), Some("nope")] {
                assert_eq!(
                    classified(Op::Install { base }, &facts),
                    (ErrorCode::AlreadyInstalled, Some("docklet start"))
                );
            }
        }
    }

    #[test]
    fn a_failed_install_of_an_unknown_base_says_so() {
        assert_eq!(
            classified(Op::Install { base: Some("arch") }, &not_installed()),
            (ErrorCode::UnknownBase, Some("docklet bases"))
        );
    }

    #[test]
    fn any_other_failed_install_is_a_plain_failure() {
        // A download or an import that failed: nothing is installed and the
        // base, if any, was a real one.
        for base in [None, Some("debian-13"), Some("ubuntu-26.04")] {
            assert_eq!(
                classified(Op::Install { base }, &not_installed()),
                (ErrorCode::Failed, None)
            );
        }
    }

    #[test]
    fn a_failed_start() {
        assert_eq!(
            classified(Op::Start, &not_installed()),
            (ErrorCode::NotInstalled, Some("docklet install"))
        );
        // dockerd never answered within the start timeout.
        assert_eq!(
            classified(Op::Start, &starting()),
            (ErrorCode::Timeout, Some("docklet doctor"))
        );
        // The keepalive could not be started, or the distro went down again.
        assert_eq!(
            classified(Op::Start, &stopped()),
            (ErrorCode::Failed, Some("docklet doctor"))
        );
        // The engine is up; what failed was the pipe proxy.
        assert_eq!(
            classified(Op::Start, &running()),
            (ErrorCode::Failed, Some("docklet doctor"))
        );
    }

    #[test]
    fn other_operations_fail_plainly() {
        for facts in [stopped(), starting(), running()] {
            assert_eq!(classified(Op::Other, &facts), (ErrorCode::Failed, None));
        }
        assert_eq!(
            classified(Op::Other, &not_installed()),
            (ErrorCode::NotInstalled, Some("docklet install"))
        );
    }
}
