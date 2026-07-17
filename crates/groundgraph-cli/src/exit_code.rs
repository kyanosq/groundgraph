//! Typed process exit codes for the GroundGraph CLI (issues.md #233).
//!
//! The contract is deliberately small so shell scripts can branch on it:
//!
//! | code | meaning    | when                                                                |
//! |------|------------|---------------------------------------------------------------------|
//! | 0    | success    | command completed.                                                  |
//! | 2    | user error | invalid argument, missing file/config/database (`groundgraph init` |
//! |      |            | first), unparseable config, requested symbol not indexed, check     |
//! |      |            | found errors, doctor found problems, partial index failure.         |
//! |      |            | Matches clap's parse-error exit code so "the user wrote the         |
//! |      |            | invocation wrong" is uniform whether clap or a runner caught it.   |
//! | 70   | internal   | unexpected failure the user cannot fix by changing the invocation  |
//! |      |            | (EX_SOFTWARE from `sysexits.h`).                                    |
//!
//! The mapping lives here, not at each `?`, so runners keep returning
//! `anyhow::Result` and the classification stays in one auditable place.

use groundgraph_engine::{EngineError, ErrorKind};

/// Process exited successfully.
pub const EXIT_SUCCESS: u8 = 0;
/// A user-correctable error (bad input / missing artefact / partial work).
pub const EXIT_USER_ERROR: u8 = 2;
/// An internal failure the user cannot fix by changing the invocation.
pub const EXIT_INTERNAL: u8 = 70;

/// A user-correctable CLI-level error that is *not* raised inside the engine
/// (where [`EngineError`] already carries the classification via
/// [`EngineError::kind`]). The runner inspects a *successful* result and
/// decides the process must still exit non-zero — e.g. a partial index
/// (#232: an indexer failed because a tool is missing) or doctor findings
/// (#116: the environment is incomplete). Construct with [`bail_user!`].
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UserError {}

/// Bail with a CLI-level user error → exit code 2 under the #233 contract.
///
/// This is the symmetric counterpart to `anyhow::bail!`: an error raised here
/// carries a [`UserError`] in its cause chain, so [`classify`] maps it to
/// exit 2 (user-correctable), whereas a bare `anyhow::bail!` — with no
/// [`EngineError`] in the chain — classifies as an internal failure (exit 70).
///
/// Use it for *argument* validation inside command runners (missing or
/// mutually exclusive flags, an unparseable `--kind`/`--purity` value, a
/// user-supplied config file whose shape is wrong). Keep `anyhow::bail!` /
/// `?` propagation for operational or internal faults the user cannot fix by
/// changing the invocation (IO failure, serialisation, SQLite layer).
macro_rules! bail_user {
    ($($arg:tt)*) => {
        return Err($crate::exit_code::UserError(::std::format!($($arg)*)).into())
    };
}
pub(crate) use bail_user;

/// Map a runner error to a process exit code under the #233 contract.
///
/// Order:
/// 1. An explicit [`UserError`] (CLI-level decision on a successful result).
/// 2. A typed [`EngineError`] — anywhere in the cause chain, because engine
///    errors usually arrive wrapped by `anyhow::Context` at the CLI boundary.
///    [`EngineError::kind`] is the classification seam this contract consumes.
/// 3. A bare `io::ErrorKind::NotFound` in the chain (a runner that read a
///    user-supplied file that is not there).
/// 4. Anything else is an internal failure.
pub fn classify(err: &anyhow::Error) -> u8 {
    if err.downcast_ref::<UserError>().is_some() {
        return EXIT_USER_ERROR;
    }
    for cause in err.chain() {
        if let Some(engine_err) = cause.downcast_ref::<EngineError>() {
            return match engine_err.kind() {
                ErrorKind::UserInput | ErrorKind::NotFound => EXIT_USER_ERROR,
                ErrorKind::Operational | ErrorKind::Internal => EXIT_INTERNAL,
            };
        }
    }
    if err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    }) {
        return EXIT_USER_ERROR;
    }
    EXIT_INTERNAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_user_error_classifies_as_2() {
        let err: anyhow::Error = UserError("partial".into()).into();
        assert_eq!(classify(&err), EXIT_USER_ERROR);
    }

    #[test]
    fn engine_user_input_classifies_as_2() {
        let err: anyhow::Error = EngineError::NoWorkspace {
            repo_root: std::path::PathBuf::from("/r"),
        }
        .into();
        assert_eq!(classify(&err), EXIT_USER_ERROR);
    }

    #[test]
    fn engine_user_input_classifies_as_2_through_anyhow_context() {
        // Runners wrap engine errors with `.context(...)`; the EngineError is
        // then buried one level down, not at the surface.
        let engine: EngineError = EngineError::InvalidInput("bad ref".into());
        let err: anyhow::Error = anyhow::Error::new(engine).context("running impact");
        assert_eq!(classify(&err), EXIT_USER_ERROR);
    }

    #[test]
    fn engine_not_found_classifies_as_2() {
        let err: anyhow::Error = EngineError::NotFound {
            what: "symbol x".into(),
        }
        .into();
        assert_eq!(classify(&err), EXIT_USER_ERROR);
    }

    #[test]
    fn engine_operational_classifies_as_70() {
        let err: anyhow::Error = EngineError::Internal(anyhow::anyhow!("boom")).into();
        assert_eq!(classify(&err), EXIT_INTERNAL);
    }

    #[test]
    fn bare_io_not_found_classifies_as_2() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: anyhow::Error = io.into();
        assert_eq!(classify(&err), EXIT_USER_ERROR);
    }

    #[test]
    fn other_io_error_classifies_as_70() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: anyhow::Error = io.into();
        assert_eq!(classify(&err), EXIT_INTERNAL);
    }

    #[test]
    fn generic_anyhow_error_classifies_as_70() {
        let err: anyhow::Error = anyhow::anyhow!("unexpected");
        assert_eq!(classify(&err), EXIT_INTERNAL);
    }

    #[test]
    fn bail_user_produces_a_classify_2_user_error() {
        // `bail_user!` is the symmetric counterpart to `anyhow::bail!`: its
        // errors classify as user-correctable (exit 2), not the internal 70
        // a bare `anyhow::bail!` (with no `EngineError` in the chain) yields.
        fn reject(input: Option<&str>) -> anyhow::Result<()> {
            match input {
                Some(_) => Ok(()),
                None => bail_user!("bad input: {}", 42),
            }
        }
        let err = reject(None).unwrap_err();
        assert_eq!(classify(&err), EXIT_USER_ERROR);
        assert_eq!(err.to_string(), "bad input: 42");
    }
}
