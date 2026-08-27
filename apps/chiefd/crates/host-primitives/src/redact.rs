//! Redaction of host-tool diagnostics.
//!
//! Plan §4: `probe_provider` runs with a scrubbed environment and **redacted
//! stderr**, and invariant 32 says a credential never travels as an argv or
//! env value (the pane's key lives in its pi-home's 0600
//! `.provider-credentials.json`). Both rules are only as good as the last
//! place a secret could still escape: the diagnostic text chiefd copies into
//! an error, a health incident or the event journal.
//!
//! This module is deliberately conservative. It never tries to decide whether
//! a token is "real" — a shape that *could* be a credential is replaced,
//! because a false positive costs a reader some context while a false negative
//! writes an API key into a durable store.

/// What a redacted span is replaced with.
const MASK: &str = "[redacted]";

/// Key names whose values are always masked, matched case-insensitively as
/// substrings so `ANTHROPIC_API_KEY` and `x-api-key` are both covered.
const SENSITIVE_KEY_FRAGMENTS: [&str; 6] =
    ["token", "key", "secret", "password", "passwd", "credential"];

/// Literal prefixes that identify a credential regardless of context.
const SECRET_PREFIXES: [&str; 4] = ["sk-", "ghp_", "github_pat_", "xoxb-"];

/// Redact a host-tool diagnostic before it is stored, logged or returned.
///
/// Three shapes are masked:
///
/// * `KEY=value` / `KEY: value` where the key names a credential;
/// * bare tokens carrying a known credential prefix (`sk-…`, `ghp_…`);
/// * nothing else — the text is otherwise preserved, because operators debug
///   from these strings.
#[must_use]
pub fn redact(input: &str) -> String {
    input.lines().map(redact_line).collect::<Vec<_>>().join("\n")
}

fn redact_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut first = true;
    // `AUTH_TOKEN: abc` splits the value into the *next* token. A dangling
    // credential name therefore arms the following token as well; forgetting
    // this is how "we redact assignments" still ships the secret.
    let mut mask_next = false;
    for token in line.split(' ') {
        if !first {
            out.push(' ');
        }
        first = false;
        if mask_next && !token.is_empty() {
            out.push_str(MASK);
            mask_next = false;
            continue;
        }
        let (rendered, dangling) = redact_token(token);
        mask_next = dangling;
        out.push_str(&rendered);
    }
    out
}

/// Returns the rendered token and whether it was a credential *name* whose
/// value has not been seen yet.
fn redact_token(token: &str) -> (String, bool) {
    for separator in ['=', ':'] {
        match mask_assignment(token, separator) {
            Assignment::Masked(masked) => return (masked, false),
            Assignment::NameOnly(name) => return (name, true),
            Assignment::NotSensitive => {}
        }
    }
    if SECRET_PREFIXES.iter().any(|prefix| token.contains(prefix)) {
        return (MASK.to_owned(), false);
    }
    (token.to_owned(), false)
}

enum Assignment {
    Masked(String),
    NameOnly(String),
    NotSensitive,
}

/// `NAME<sep>value` where `NAME` looks like a credential name.
fn mask_assignment(token: &str, separator: char) -> Assignment {
    let Some((name, value)) = token.split_once(separator) else {
        return Assignment::NotSensitive;
    };
    let lowered = name.to_lowercase();
    if !SENSITIVE_KEY_FRAGMENTS.iter().any(|fragment| lowered.contains(fragment)) {
        return Assignment::NotSensitive;
    }
    if value.is_empty() {
        Assignment::NameOnly(format!("{name}{separator}"))
    } else {
        Assignment::Masked(format!("{name}{separator}{MASK}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_named_assignments_lose_their_value_but_keep_their_name() {
        assert_eq!(redact("ANTHROPIC_API_KEY=sk-abc123"), "ANTHROPIC_API_KEY=[redacted]");
        assert_eq!(redact("x-api-key: abcdef"), "x-api-key: [redacted]");
        assert_eq!(redact("PROVIDER_BOT_TOKEN=99:AA"), "PROVIDER_BOT_TOKEN=[redacted]");
    }

    #[test]
    fn bare_credentials_are_masked_even_without_a_key_name() {
        assert_eq!(redact("failed for sk-live-1234567890"), "failed for [redacted]");
        assert_eq!(redact("ghp_deadbeef rejected"), "[redacted] rejected");
    }

    #[test]
    fn ordinary_diagnostics_survive_unchanged_because_operators_read_them() {
        let detail = "runtime: can't find session: cobalt";
        assert_eq!(redact(detail), detail);
        assert_eq!(redact("exit status 1"), "exit status 1");
    }

    #[test]
    fn every_line_is_redacted_not_just_the_first() {
        let redacted = redact("line one\nAPI_KEY=sk-secret\nline three");
        assert_eq!(redacted, "line one\nAPI_KEY=[redacted]\nline three");
        assert!(!redacted.contains("sk-secret"));
    }
}
