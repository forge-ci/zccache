//! Registered environment-variable policy for zccache-owned boolean switches.
//!
//! Most registered switches use the coherent `{1, true}` grammar. Native
//! crash capture is a documented, temporary default-on exception with `=0`
//! opt-out. Foreign variables retain their own parsing semantics.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, Ordering};

use super::COLOCATE_ENV;
use crate::defender::QUIET_ENV;

/// Value policy for a registered environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentVariableKind {
    /// A zccache-owned boolean: only `1` or case-insensitive `true` enables.
    OwnedBoolean,
    /// Enabled by default; `0` is a temporary explicit opt-out.
    DefaultOnBoolean,
}

/// One registered zccache environment variable and its documented policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentVariableDeclaration {
    /// Stable environment-variable name.
    pub name: &'static str,
    /// How this variable's value is interpreted.
    pub kind: EnvironmentVariableKind,
    /// Concise operator-facing purpose.
    pub help: &'static str,
}

/// Set this to bypass zccache completely for one process invocation.
const ZCCACHE_DISABLE_ENV: &str = "ZCCACHE_DISABLE";
/// Set this in an embedding host to forbid standalone daemon launches.
pub const NO_SPAWN_ENV: &str = "ZCCACHE_NO_SPAWN";
/// Set this to run cheap compiler probes without an IPC round trip.
const ZCCACHE_PROBE_BYPASS_ENV: &str = "ZCCACHE_PROBE_BYPASS";
/// Set this to allow caching Rust `--test` harness links.
pub const CACHE_TEST_BINS_ENV: &str = "ZCCACHE_CACHE_TEST_BINS";
/// Set this to `0` to opt out of daemon native crash capture temporarily.
pub const NATIVE_CRASH_CAPTURE_ENV: &str = "ZCCACHE_NATIVE_CRASH_CAPTURE";

/// Set this to turn every materialized-output mtime floor off.
const MTIME_FLOOR_DISABLE_ENV: &str = "ZCCACHE_DISABLE_MTIME_FLOOR";
/// Set this to store artifacts as single pack files.
const PACK_ARTIFACTS_ENV: &str = "ZCCACHE_PACK_ARTIFACTS";
/// Set this to print one CWD/argv diagnostic per wrapper invocation.
const DIAG_CWD_ENV: &str = "ZCCACHE_DIAG_CWD";
/// Set this to expand the session summary.
const SUMMARY_VERBOSE_ENV: &str = "ZCCACHE_SUMMARY_VERBOSE";
/// Set this to restore fail-fast behaviour for lost link requests.
const LINK_RETRY_DISABLE_ENV: &str = "ZCCACHE_DISABLE_LINK_RETRY";

/// The complete registry for zccache-owned boolean switches.
///
/// New entries require a typed accessor below. The `ban_registered_env_read`
/// Dylint rejects direct `std::env::{var,var_os}` reads of these names outside
/// this owner, preserving one parser and one lookup point per policy.
pub const ENVIRONMENT_VARIABLES: &[EnvironmentVariableDeclaration] = &[
    EnvironmentVariableDeclaration {
        name: ZCCACHE_DISABLE_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Bypass zccache and run the compiler directly.",
    },
    EnvironmentVariableDeclaration {
        name: NO_SPAWN_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Forbid standalone zccache daemon launches from an embedding host.",
    },
    EnvironmentVariableDeclaration {
        name: ZCCACHE_PROBE_BYPASS_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Run cheap compiler probes without using the cache daemon.",
    },
    EnvironmentVariableDeclaration {
        name: CACHE_TEST_BINS_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Opt into caching Rust test-harness links.",
    },
    EnvironmentVariableDeclaration {
        name: NATIVE_CRASH_CAPTURE_ENV,
        kind: EnvironmentVariableKind::DefaultOnBoolean,
        help: "Temporarily disable default-on daemon native crash capture with 0.",
    },
    EnvironmentVariableDeclaration {
        name: QUIET_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Silence the daemon's Windows Defender exclusion banner.",
    },
    EnvironmentVariableDeclaration {
        name: MTIME_FLOOR_DISABLE_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Turn off sibling mtime floors on materialized outputs.",
    },
    EnvironmentVariableDeclaration {
        name: COLOCATE_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Root the cache on the working directory's volume.",
    },
    EnvironmentVariableDeclaration {
        name: PACK_ARTIFACTS_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Store artifacts as single pack files.",
    },
    EnvironmentVariableDeclaration {
        name: DIAG_CWD_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Print one CWD/argv diagnostic per wrapper invocation.",
    },
    EnvironmentVariableDeclaration {
        name: SUMMARY_VERBOSE_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Expand the session summary.",
    },
    EnvironmentVariableDeclaration {
        name: LINK_RETRY_DISABLE_ENV,
        kind: EnvironmentVariableKind::OwnedBoolean,
        help: "Fail fast instead of retrying a lost link request.",
    },
];

#[derive(Debug, Clone, Copy)]
enum OwnedBoolean {
    Disable,
    NoSpawn,
    ProbeBypass,
    CacheTestBinaries,
    Quiet,
    MtimeFloorDisable,
    Colocate,
    PackArtifacts,
    DiagCwd,
    SummaryVerbose,
    LinkRetryDisable,
}

const OWNED_BOOLEAN_COUNT: usize = 11;

/// One flag per [`OwnedBoolean`], so a polled switch warns once per process.
static WARNED: [AtomicBool; OWNED_BOOLEAN_COUNT] =
    [const { AtomicBool::new(false) }; OWNED_BOOLEAN_COUNT];

impl OwnedBoolean {
    const fn name(self) -> &'static str {
        match self {
            Self::Disable => ZCCACHE_DISABLE_ENV,
            Self::NoSpawn => NO_SPAWN_ENV,
            Self::ProbeBypass => ZCCACHE_PROBE_BYPASS_ENV,
            Self::CacheTestBinaries => CACHE_TEST_BINS_ENV,
            Self::Quiet => QUIET_ENV,
            Self::MtimeFloorDisable => MTIME_FLOOR_DISABLE_ENV,
            Self::Colocate => COLOCATE_ENV,
            Self::PackArtifacts => PACK_ARTIFACTS_ENV,
            Self::DiagCwd => DIAG_CWD_ENV,
            Self::SummaryVerbose => SUMMARY_VERBOSE_ENV,
            Self::LinkRetryDisable => LINK_RETRY_DISABLE_ENV,
        }
    }

    fn enabled(self) -> bool {
        if matches!(self, Self::NoSpawn) {
            return no_spawn_from_env_value(std::env::var_os(self.name()).as_deref());
        }
        let value = std::env::var(self.name()).ok();
        if owned_flag_value_is_unrecognized(value.as_deref())
            && !WARNED[self as usize].swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                variable = self.name(),
                value = value.as_deref().unwrap_or_default(),
                "unrecognized value leaves this switch off; use 1 or true to enable it"
            );
        }
        owned_flag_enabled(value.as_deref())
    }
}

/// True when a set value is neither an enabling spelling nor a plain off
/// spelling, so the operator probably expected something else to happen.
/// `yes` and `on` land here: they read as "on" but this grammar disables them.
fn owned_flag_value_is_unrecognized(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        let trimmed = raw.trim();
        !trimmed.is_empty()
            && !owned_flag_enabled(Some(trimmed))
            && !["0", "false", "no", "off"]
                .iter()
                .any(|off| trimmed.eq_ignore_ascii_case(off))
    })
}

/// The canonical grammar for a zccache-owned boolean switch: `1` or
/// case-insensitive `true` enables it; every other value, including an
/// unrecognised one, is disabled. Whitespace around a valid value is ignored.
#[must_use]
pub fn owned_flag_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        let trimmed = raw.trim();
        trimmed == "1" || trimmed.eq_ignore_ascii_case("true")
    })
}

/// Applies [`owned_flag_enabled`] to an environment variable by name.
///
/// This compatibility shim preserves the former public
/// `zccache::core::config::owned_env_flag_enabled` API. New internal callers
/// must use the typed accessor for their registered setting so the registry
/// remains the sole owner of lookup and parsing policy.
#[deprecated(
    note = "use the typed zccache_core::config accessor for the registered environment variable"
)]
#[must_use]
pub fn owned_env_flag_enabled(name: &str) -> bool {
    owned_flag_enabled(std::env::var(name).ok().as_deref())
}

/// True when `ZCCACHE_DISABLE` bypasses the cache for this process.
#[must_use]
pub fn zccache_disabled() -> bool {
    OwnedBoolean::Disable.enabled()
}

/// True when the host forbids standalone daemon spawns via [`NO_SPAWN_ENV`].
#[must_use]
pub fn daemon_spawn_disabled() -> bool {
    OwnedBoolean::NoSpawn.enabled()
}

/// True when `ZCCACHE_PROBE_BYPASS` opts cheap probes out of the daemon path.
#[must_use]
pub fn probe_bypass_enabled() -> bool {
    OwnedBoolean::ProbeBypass.enabled()
}

/// True when `ZCCACHE_CACHE_TEST_BINS` re-admits Rust `--test` harness links.
#[must_use]
pub fn cache_test_binaries_enabled() -> bool {
    OwnedBoolean::CacheTestBinaries.enabled()
}

/// True when `ZCCACHE_QUIET` silences the Defender exclusion banner.
#[must_use]
pub fn quiet_enabled() -> bool {
    OwnedBoolean::Quiet.enabled()
}

/// True when `ZCCACHE_DISABLE_MTIME_FLOOR` asks to turn mtime floors off.
#[must_use]
pub fn mtime_floor_disable_requested() -> bool {
    OwnedBoolean::MtimeFloorDisable.enabled()
}

/// True when `ZCCACHE_COLOCATE` opts into a same-volume cache root.
#[must_use]
pub fn colocate_requested() -> bool {
    OwnedBoolean::Colocate.enabled()
}

/// True when `ZCCACHE_PACK_ARTIFACTS` selects pack-file artifact storage.
#[must_use]
pub fn pack_artifacts_enabled() -> bool {
    OwnedBoolean::PackArtifacts.enabled()
}

/// True when `ZCCACHE_DIAG_CWD` asks for the wrapper CWD/argv diagnostic.
#[must_use]
pub fn diag_cwd_enabled() -> bool {
    OwnedBoolean::DiagCwd.enabled()
}

/// True when `ZCCACHE_SUMMARY_VERBOSE` expands the session summary.
#[must_use]
pub fn summary_verbose_enabled() -> bool {
    OwnedBoolean::SummaryVerbose.enabled()
}

/// True when `ZCCACHE_DISABLE_LINK_RETRY` opts out of lost-link retries.
#[must_use]
pub fn link_retry_disabled() -> bool {
    OwnedBoolean::LinkRetryDisable.enabled()
}

/// True unless `ZCCACHE_NATIVE_CRASH_CAPTURE=0` temporarily opts the daemon
/// out of native crash capture for one zccache release after re-enablement.
#[must_use]
pub fn native_crash_capture_enabled() -> bool {
    native_crash_capture_from_env_value(std::env::var(NATIVE_CRASH_CAPTURE_ENV).ok().as_deref())
}

fn native_crash_capture_from_env_value(value: Option<&str>) -> bool {
    value.is_none_or(|raw| raw.trim() != "0")
}

/// Testable core of [`daemon_spawn_disabled`] — no environment access.
#[must_use]
pub(crate) fn no_spawn_from_env_value(value: Option<&OsStr>) -> bool {
    owned_flag_enabled(value.map(|value| value.to_string_lossy()).as_deref())
}

/// Standard error for a refused spawn. Names [`NO_SPAWN_ENV`] so operators
/// can find the knob, and points at the embedded service so the failure is
/// self-explaining in host contexts.
#[must_use]
pub fn no_spawn_error(daemon_name: &str) -> String {
    format!(
        "{daemon_name} spawn disabled by host ({NO_SPAWN_ENV}=1); \
         this host serves compiles through an embedded zccache service"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_records_native_capture_as_default_on() {
        assert_eq!(ENVIRONMENT_VARIABLES.len(), OWNED_BOOLEAN_COUNT + 1);
        assert!(ENVIRONMENT_VARIABLES
            .iter()
            .filter(|declaration| declaration.name != NATIVE_CRASH_CAPTURE_ENV)
            .all(|declaration| declaration.kind == EnvironmentVariableKind::OwnedBoolean));
        assert_eq!(
            ENVIRONMENT_VARIABLES[4].kind,
            EnvironmentVariableKind::DefaultOnBoolean
        );
    }

    #[test]
    fn every_owned_accessor_name_is_registered_as_owned() {
        let accessors = [
            OwnedBoolean::Disable,
            OwnedBoolean::NoSpawn,
            OwnedBoolean::ProbeBypass,
            OwnedBoolean::CacheTestBinaries,
            OwnedBoolean::Quiet,
            OwnedBoolean::MtimeFloorDisable,
            OwnedBoolean::Colocate,
            OwnedBoolean::PackArtifacts,
            OwnedBoolean::DiagCwd,
            OwnedBoolean::SummaryVerbose,
            OwnedBoolean::LinkRetryDisable,
        ];
        assert_eq!(accessors.len(), OWNED_BOOLEAN_COUNT);
        for accessor in accessors {
            assert!(
                ENVIRONMENT_VARIABLES.iter().any(|declaration| {
                    declaration.name == accessor.name()
                        && declaration.kind == EnvironmentVariableKind::OwnedBoolean
                }),
                "{} is not registered",
                accessor.name()
            );
        }
    }

    /// #1969: unset, empty, whitespace, case, each paired spelling and unknown
    /// strings, with whether each value enables the switch and whether it warns.
    #[test]
    fn owned_grammar_table() {
        let table: &[(Option<&str>, bool, bool)] = &[
            (None, false, false),
            (Some(""), false, false),
            (Some("   "), false, false),
            (Some("\t\n"), false, false),
            (Some("1"), true, false),
            (Some(" 1 "), true, false),
            (Some("true"), true, false),
            (Some("TRUE"), true, false),
            (Some(" True\n"), true, false),
            (Some("0"), false, false),
            (Some("false"), false, false),
            (Some("FALSE"), false, false),
            (Some("off"), false, false),
            (Some("OFF"), false, false),
            (Some("no"), false, false),
            (Some("NO"), false, false),
            (Some("yes"), false, true),
            (Some("YES"), false, true),
            (Some("on"), false, true),
            (Some("ON"), false, true),
            (Some("maybe"), false, true),
            (Some("2"), false, true),
            (Some("1 1"), false, true),
            (Some("ture"), false, true),
        ];
        for (value, enabled, warns) in table.iter().copied() {
            assert_eq!(owned_flag_enabled(value), enabled, "enabled: {value:?}");
            assert_eq!(
                owned_flag_value_is_unrecognized(value),
                warns,
                "warns: {value:?}"
            );
        }
    }

    #[test]
    fn registry_names_are_unique() {
        for (index, declaration) in ENVIRONMENT_VARIABLES.iter().enumerate() {
            assert!(
                ENVIRONMENT_VARIABLES[..index]
                    .iter()
                    .all(|previous| previous.name != declaration.name),
                "duplicate registered environment variable: {}",
                declaration.name,
            );
        }
    }

    #[test]
    fn native_capture_is_default_on_with_zero_opt_out() {
        assert!(native_crash_capture_from_env_value(None));
        assert!(!native_crash_capture_from_env_value(Some("0")));
        assert!(native_crash_capture_from_env_value(Some("1")));
    }

    #[test]
    #[allow(deprecated)]
    fn compatibility_accessor_keeps_the_former_dynamic_lookup() {
        const COMPATIBILITY_TEST_ENV: &str = "ZCCACHE_ISSUE_1478_COMPATIBILITY_TEST";

        std::env::remove_var(COMPATIBILITY_TEST_ENV);
        assert!(!owned_env_flag_enabled(COMPATIBILITY_TEST_ENV));

        std::env::set_var(COMPATIBILITY_TEST_ENV, " true ");
        assert!(owned_env_flag_enabled(COMPATIBILITY_TEST_ENV));

        std::env::remove_var(COMPATIBILITY_TEST_ENV);
    }
}
