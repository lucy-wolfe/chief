//! How beacond is configured.

/// Default bind address. Below chiefd's port-walk range (8792+) so a walking
/// chiefd can never land on the discovery service. Loopback because beacond
/// has no auth (see the crate's module doc).
///
/// THE ONLY PLACE THE DISCOVERY ADDRESS IS WRITTEN DOWN, and that is enforced
/// by `scripts/test/beacond-port-single-definition.test.mjs`. It used to be
/// written down four times — here, in [`usage`]'s environment table, in
/// `chiefd::beacon`'s `DEFAULT_BEACOND_URL` and again in
/// `chiefd::lifecycle::discovery`'s. The cost of that is on the record:
/// `lifecycle/discovery.rs`'s `unreachable_beacond_detail` exists because an
/// INSTALLED beacond predating a port move started perfectly, bound the old
/// address, and was never found — a binary that contained no occurrence of
/// this port at all. Every consumer reads [`DEFAULT_BIND`] or [`default_url`];
/// nobody re-types the number.
pub const DEFAULT_BIND: &str = "127.0.0.1:6969";

/// The loopback URL a client dials to reach a beacond bound at
/// [`DEFAULT_BIND`], derived from it rather than spelled again.
///
/// A function and not a `const` because stable Rust has no const string
/// concatenation, and a second literal is exactly what this is here to
/// prevent. It is called once per process at client construction, next to an
/// env-var read — the allocation is not on any hot path.
#[must_use]
pub fn default_url() -> String {
    format!("http://{DEFAULT_BIND}")
}

/// What a host that cannot name itself is recorded as in a company's
/// registration.
///
/// THE ONE DEFINITION, and it lives here for the same reason [`DEFAULT_BIND`]
/// does: it is a value one program WRITES into this registry and another READS
/// back out of it, and the two must agree on the exact string or every
/// registration reads as foreign and no company can be started again. Those two
/// programs used to be one binary and shared a `pub(crate)` constant; the P6
/// operator-client split divided them into `chiefd` (which reports the
/// hostname) and `chiefd` (which judges liveness against it), and the client
/// links none of the daemon's crates — so a constant in
/// either one would immediately have become a second copy in the other.
/// beacond is the registry both of them already depend on, and the sentinel is
/// its column's vocabulary, not either caller's.
pub const UNNAMEABLE_HOST: &str = "unknown";

/// Env var overriding [`DEFAULT_BIND`].
pub const BIND_ENV: &str = "BEACOND_BIND";

/// Env var overriding the default registry path.
pub const DB_PATH_ENV: &str = "BEACOND_DB_PATH";

/// What the command line asked for.
///
/// `beacond` used to recognise only `--version`/`-V` and fall through to
/// "bind a port and serve" for EVERY other argument, so `beacond --help`
/// started a daemon on 127.0.0.1:6969 and never returned. A typo'd flag did
/// the same thing silently. An argument the program does not understand must
/// never be answered by starting a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// No arguments: run the discovery service.
    Serve,
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help`.
    Help,
    /// Anything else, carrying the offending argument for the error message.
    Unknown(String),
}

/// Usage text for [`Invocation::Help`]. Names the environment because that is
/// beacond's whole configuration surface — it takes no flags.
///
/// Built, not stored: the default it quotes is interpolated from
/// [`DEFAULT_BIND`] instead of retyped. A `const` could only have carried a
/// second copy of the address, which is the defect class this whole file's
/// single-definition rule exists to close — the test below asserted the two
/// agreed, but an assertion that two literals match is a slower way to have
/// one literal.
#[must_use]
pub fn usage() -> String {
    format!(
        concat!(
            "beacond — company discovery for chiefd.\n",
            "\n",
            "USAGE:\n",
            "    beacond            run the discovery service in the foreground\n",
            "    beacond --help     print this message\n",
            "    beacond --version  print the version\n",
            "\n",
            "beacond takes no flags; it is configured entirely by environment:\n",
            "    BEACOND_BIND       address to bind (default {default_bind})\n",
            "    BEACOND_DB_PATH    registry database (default $HOME/.chief/beacond.sqlite)\n",
        ),
        default_bind = DEFAULT_BIND
    )
}

/// Classify the argument list. Pure, so the dispatch is unit-testable without
/// spawning the binary or binding a port.
#[must_use]
pub fn classify_args<I, S>(args: I) -> Invocation
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Invocation::Serve;
    };
    match first.as_ref() {
        "--version" | "-V" => Invocation::Version,
        "--help" | "-h" | "help" => Invocation::Help,
        other => Invocation::Unknown(other.to_owned()),
    }
}

/// How beacond is configured. Resolved from an INJECTED lookup, not
/// `std::env` directly, so precedence is testable under a parallel runner
/// (the same shape as `chiefd_api::docstore::Config::from_env`).
///
/// TWO fields, and there is no third. It carried a `data_root`/`orgs_root`
/// pair — `$HOME/.chiefd` and its `orgs` tree — served on `GET /v1/data-root`
/// so no client had to carry a copy of the path policy. A company is the
/// directory the operator ran `chief` in, so there is no root to be the
/// authority on: the policy, the route and the whole resolution are deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Address to bind. Default [`DEFAULT_BIND`].
    pub bind: String,
    /// The registry database file. Default `$HOME/.chief/beacond.sqlite`.
    ///
    /// The one file beacond owns, and the one thing still in `~`: the list of
    /// companies cannot live inside a company (the bootstrap paradox), so it
    /// sits beside the box's other install-level facts in `~/.chief`.
    pub db_path: String,
}

/// `BEACOND_DB_PATH` is unset and `$HOME` cannot be read, so there is no
/// place to put the registry.
///
/// Why a default path is right here, when chiefd's own store deliberately has
/// none: chiefd refuses to guess because an empty org store looks healthy
/// while every document is gone. The same reasoning applies to beacond in the
/// opposite direction — this file is durable and authoritative (it is the
/// list of companies, ruling D21), so it must land in one predictable place
/// rather than wherever a caller's cwd happened to be. A single default with
/// an explicit override is how "which file is the registry" stops being a
/// guess.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `HOME` cannot be read, so the registry has no place to live.
    #[error(
        "beacond needs a registry path: set BEACOND_DB_PATH, or HOME so it can default to \
         $HOME/.chief/beacond.sqlite"
    )]
    MissingHome,
}

impl Config {
    /// Resolve configuration from an injected environment lookup.
    ///
    /// # Errors
    /// [`ConfigError::MissingHome`] when `HOME` cannot be read.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let bind = var(BIND_ENV).unwrap_or_else(|| DEFAULT_BIND.to_string());
        let db_path = match var(DB_PATH_ENV) {
            Some(path) => path,
            None => {
                format!("{}/.chief/beacond.sqlite", var("HOME").ok_or(ConfigError::MissingHome)?)
            }
        };
        Ok(Self { bind, db_path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn explicit_bind_and_db_path_win() {
        let config = Config::from_env(env(&[
            (BIND_ENV, "127.0.0.1:9999"),
            (DB_PATH_ENV, "/tmp/explicit.sqlite"),
            ("HOME", "/root"),
        ]))
        .expect("config resolves");
        assert_eq!(config.bind, "127.0.0.1:9999");
        assert_eq!(config.db_path, "/tmp/explicit.sqlite");
    }

    /// The registry sits in `~/.chief`, beside the box's other install-level
    /// facts — not in `~/.chiefd`, which was the global company tree and is
    /// gone with it.
    #[test]
    fn defaults_fill_in_from_home() {
        let config = Config::from_env(env(&[("HOME", "/root")])).expect("config resolves");
        assert_eq!(config.bind, DEFAULT_BIND);
        assert_eq!(config.db_path, "/root/.chief/beacond.sqlite");
    }

    #[test]
    fn neither_db_path_nor_home_is_missing_home() {
        let result = Config::from_env(env(&[]));
        assert!(result.is_err());
    }

    /// `HOME` is read ONLY to build the default path, so a caller that names
    /// the registry file outright needs no home at all. It was read
    /// unconditionally, because the deleted data-root resolution needed it
    /// even when the database path did not.
    #[test]
    fn an_explicit_db_path_needs_no_home() {
        let config = Config::from_env(env(&[(DB_PATH_ENV, "/tmp/explicit.sqlite")]))
            .expect("config resolves without HOME");
        assert_eq!(config.db_path, "/tmp/explicit.sqlite");
    }
}

#[cfg(test)]
mod invocation_tests {
    use super::*;

    #[test]
    fn no_arguments_serves() {
        assert_eq!(classify_args(Vec::<String>::new()), Invocation::Serve);
    }

    #[test]
    fn version_flags_are_recognised() {
        assert_eq!(classify_args(["--version"]), Invocation::Version);
        assert_eq!(classify_args(["-V"]), Invocation::Version);
    }

    #[test]
    fn help_flags_are_recognised() {
        assert_eq!(classify_args(["--help"]), Invocation::Help);
        assert_eq!(classify_args(["-h"]), Invocation::Help);
        assert_eq!(classify_args(["help"]), Invocation::Help);
    }

    /// The regression this dispatch exists for: `beacond --help` bound
    /// 127.0.0.1:6969 and blocked forever, because every unrecognised
    /// argument fell through to "serve". Anything the program does not
    /// understand must classify as `Unknown` and never as `Serve`.
    #[test]
    fn an_unrecognised_argument_never_classifies_as_serve() {
        for argument in ["--halp", "--bind", "serve", "-x", ""] {
            let invocation = classify_args([argument]);
            assert_eq!(
                invocation,
                Invocation::Unknown(argument.to_owned()),
                "{argument:?} must be rejected, not answered by starting a server"
            );
        }
    }

    #[test]
    fn usage_names_every_environment_key_that_configures_beacond() {
        let usage = usage();
        assert!(usage.contains(BIND_ENV));
        assert!(usage.contains(DB_PATH_ENV));
        assert!(usage.contains(DEFAULT_BIND));
    }

    /// The URL half of the address is DERIVED, so the two can never disagree.
    ///
    /// They did disagree in kind if not in value: `chiefd::beacon` and
    /// `chiefd::lifecycle::discovery` each carried their own
    /// `DEFAULT_BEACOND_URL` literal, and `lifecycle/discovery.rs`'s
    /// `unreachable_beacond_detail` is the message written for the day an
    /// installed binary disagreed with this constant about which port
    /// discovery answers on.
    #[test]
    fn the_dialled_url_is_derived_from_the_bound_address() {
        assert_eq!(default_url(), format!("http://{DEFAULT_BIND}"));
        assert!(default_url().ends_with(DEFAULT_BIND));
        assert!(default_url().starts_with("http://"));
    }
}
