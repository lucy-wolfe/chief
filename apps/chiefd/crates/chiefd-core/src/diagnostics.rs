//! Bounding and redacting the diagnostic strings stores are allowed to keep.
//!
//! Exact port of `boundedPersistedError` (`src/organization/org-diagnostics.ts`).
//! Two M10 stores depend on it and would be wrong without it:
//!
//! * **health** calls it `redact()` and runs *every* incident detail through it
//!   — plan §2.7: *"all detail through `redact()`"*. The incident fingerprint is
//!   `sha256(kind\ndetail)` of the **redacted** detail, so a drifting redactor
//!   silently re-fingerprints every incident and the dedup/resolution machinery
//!   stops matching its own history.
//! * **session maintenance** bounds `request.error` with it on write *and* on
//!   read-repair, and the ledger validator rejects an error longer than 600
//!   characters. A shorter or longer bound here is a validation failure on a
//!   ledger chiefd itself wrote.
//!
//! # Why `regex`
//!
//! Plan §0 lists the crates chiefd builds on and `regex` is not among them.
//! This module adds it deliberately (recorded in `DECISIONS.md`): the function
//! being ported is six ordered regular expressions whose *exact* behaviour is a
//! security property, and hand-rolling six scanners to match JavaScript regex
//! semantics is precisely the kind of "looks right" port the conformance corpus
//! exists to prevent. `regex` is linear-time by construction and has no
//! backtracking, so it adds no denial-of-service surface to a store path.

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

/// Persisted failures are operational hints, never transcript archives.
pub const MAX_PERSISTED_ERROR_LENGTH: usize = 600;

/// Input past this point is not even examined: a megabyte of stdout is not a
/// diagnostic.
const MAX_DIAGNOSTIC_INPUT_LENGTH: usize = 8_192;

const SECRET_LABEL: &str =
    "bearer|token|api[_-]?key|authorization|password|secret|chat[_-]?id|cookie|session[_-]?key";
const CONTENT_LABEL: &str =
    "stdout|command[_ -]?output|ledger|payload|request[_ -]?body|response[_ -]?body";

/// The suffix a truncated diagnostic carries, so a reader can tell the
/// difference between "this is the whole error" and "this is the front of it".
const TRUNCATION_SUFFIX: &str = "… [details omitted]";

struct Patterns {
    url: Regex,
    bot_token: Regex,
    bearer: Regex,
    labelled_secret: Regex,
    quoted_secret: Regex,
    content_body: Regex,
    control: Regex,
    whitespace: Regex,
}

/// The compiled pattern set, or `None` if any pattern failed to build.
///
/// `None` is not a "skip redaction" path: [`bounded_persisted_error`] answers
/// `"Diagnostic unavailable"` instead. Every pattern is a literal of this file
/// and every one is exercised by the tests below, so `None` means a typo
/// reached `main` — and the safe direction for a typo in a redactor is to
/// publish no text at all, never to publish unfiltered text. This is also how
/// the module avoids `unwrap`/`expect`/`panic`, which the workspace denies.
fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                url: compile(r"(?i)https?://\S+")?,
                bot_token: compile(r"\b\d{8,}:[A-Za-z0-9_-]{20,}\b")?,
                bearer: compile(r"(?i)\bbearer\s+\S+")?,
                labelled_secret: compile(&format!(
                    r"(?i)\b({SECRET_LABEL})\s*[:=]\s*(?:bearer\s+)?\S+"
                ))?,
                quoted_secret: compile(&format!(
                    r#"(?i)(["'](?:{SECRET_LABEL})["']\s*:\s*)(?:"[^"]*"|'[^']*'|[^,\s}}\]]+)"#
                ))?,
                content_body: compile(&format!(r"(?i)\b({CONTENT_LABEL})\s*[:=].*$"))?,
                control: compile(r"[\x00-\x1f\x7f]+")?,
                whitespace: compile(r"\s+")?,
            })
        })
        .as_ref()
}

fn compile(pattern: &str) -> Option<Regex> {
    RegexBuilder::new(pattern).size_limit(1 << 20).build().ok()
}

/// One compact, content-free diagnostic line.
///
/// Newlines are a hard boundary because launcher errors commonly put stderr
/// first and append command stdout or serialized state on later lines.
#[must_use]
pub fn bounded_persisted_error(raw: &str) -> String {
    let Some(p) = patterns() else {
        return "Diagnostic unavailable".to_string();
    };

    // First line only, with a trailing CR trimmed off a CRLF ending.
    let line_end = match raw.find('\n') {
        Some(index) if index > 0 && raw.as_bytes()[index - 1] == b'\r' => index - 1,
        Some(index) => index,
        None => raw.len(),
    };
    let cut = line_end.min(MAX_DIAGNOSTIC_INPUT_LENGTH);
    let first_line = &raw[..floor_char_boundary(raw, cut)];

    let step = p.url.replace_all(first_line, "[redacted-url]");
    let step = p.bot_token.replace_all(&step, "[redacted-token]");
    let step = p.bearer.replace_all(&step, "Bearer [redacted]");
    let step = p.labelled_secret.replace_all(&step, "$1=[redacted]");
    let step = p.quoted_secret.replace_all(&step, "$1[redacted]");
    // `.replace` (not `replace_all`): the TypeScript regex has no `g` flag, so
    // only the first labelled body is elided — and it elides to end of line.
    let step = p.content_body.replace(&step, "$1=[omitted]");
    let step = p.control.replace_all(&step, " ");
    let step = p.whitespace.replace_all(&step, " ");
    let mut safe = step.trim().to_string();

    if safe.starts_with('[') || safe.starts_with('{') {
        safe = "Structured diagnostic omitted".to_string();
    }
    if safe.is_empty() {
        safe = "Diagnostic unavailable".to_string();
    }
    if safe.chars().count() <= MAX_PERSISTED_ERROR_LENGTH {
        return safe;
    }
    let keep = MAX_PERSISTED_ERROR_LENGTH - TRUNCATION_SUFFIX.chars().count();
    let head: String = safe.chars().take(keep).collect();
    format!("{head}{TRUNCATION_SUFFIX}")
}

/// Largest index `<= at` that is a UTF-8 boundary.
///
/// The TypeScript operates on UTF-16 code units and can therefore slice
/// mid-character; Rust cannot, and rounding *down* is the safe direction (it
/// keeps strictly less input).
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut index = at.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_sentence_survives_unchanged() {
        assert_eq!(
            bounded_persisted_error("The provider refused the compaction request."),
            "The provider refused the compaction request."
        );
    }

    #[test]
    fn only_the_first_line_is_kept_because_stdout_is_appended_after_it() {
        assert_eq!(bounded_persisted_error("runtime failed\npane 1\npane 2"), "runtime failed");
        assert_eq!(bounded_persisted_error("runtime failed\r\ntrailing"), "runtime failed");
    }

    #[test]
    fn urls_bot_tokens_and_bearer_credentials_are_masked() {
        assert_eq!(
            bounded_persisted_error("GET https://api.example.com/bot123/getUpdates failed"),
            "GET [redacted-url] failed"
        );
        assert_eq!(
            bounded_persisted_error("token 123456789:AAHfSHFdsfhsdkjfhKJHkjhKJHkjh1 rejected"),
            "token [redacted-token] rejected",
            "a bot-token shape is masked even where no `key=` label precedes it"
        );
        assert_eq!(
            bounded_persisted_error("auth used Bearer sk-abcdefghijklmnop"),
            "auth used Bearer [redacted]"
        );
    }

    #[test]
    fn a_labelled_secret_is_masked_whatever_the_separator() {
        for input in [
            "api_key=abcdef123456",
            "API-KEY: abcdef123456",
            "password = hunter2",
            "chat_id: 111111111",
        ] {
            let out = bounded_persisted_error(input);
            assert!(out.contains("[redacted]"), "{input} -> {out}");
            assert!(!out.contains("hunter2"), "{input} -> {out}");
            assert!(!out.contains("abcdef123456"), "{input} -> {out}");
            assert!(!out.contains("111111111"), "{input} -> {out}");
        }
    }

    #[test]
    fn a_quoted_json_secret_key_is_masked_without_losing_the_key_name() {
        let out = bounded_persisted_error(r#"failed with "authorization": "Bearer abc" ok"#);
        assert!(!out.contains("abc"), "{out}");
        assert!(out.contains("authorization"), "{out}");
    }

    #[test]
    fn a_labelled_body_is_omitted_to_the_end_of_the_line() {
        assert_eq!(
            bounded_persisted_error("write failed stdout=the whole 4mb ledger body"),
            "write failed stdout=[omitted]",
            "the elision runs to end of line, so nothing after the label survives"
        );
    }

    /// Parity table captured by running the TypeScript `boundedPersistedError`
    /// over exactly these inputs. This is the test that makes the port a port:
    /// the health incident fingerprint is a hash of this function's output, so
    /// any drift silently re-fingerprints every stored incident.
    #[test]
    fn output_is_byte_identical_to_the_typescript_bounded_persisted_error() {
        let table = [
            (
                "The provider refused the compaction request.",
                "The provider refused the compaction request.",
            ),
            ("runtime failed\npane 1", "runtime failed"),
            ("runtime failed\r\ntrailing", "runtime failed"),
            ("GET https://api.example.com/bot123/getUpdates failed", "GET [redacted-url] failed"),
            (
                "token 123456789:AAHfSHFdsfhsdkjfhKJHkjhKJHkjh1 rejected",
                "token [redacted-token] rejected",
            ),
            ("auth used Bearer sk-abcdefghijklmnop", "auth used Bearer [redacted]"),
            ("api_key=abcdef123456", "api_key=[redacted]"),
            ("API-KEY: abcdef123456", "API-KEY=[redacted]"),
            ("password = hunter2", "password=[redacted]"),
            ("chat_id: 111111111", "chat_id=[redacted]"),
            (
                r#"failed with "authorization": "Bearer abc" ok"#,
                r#"failed with "authorization": [redacted] [redacted] ok"#,
            ),
            ("write failed stdout=the whole 4mb ledger body", "write failed stdout=[omitted]"),
            (r#"{"assignments":[{"id":"a"}]}"#, "Structured diagnostic omitted"),
            ("[1,2,3]", "Structured diagnostic omitted"),
            ("", "Diagnostic unavailable"),
            (" \t  ", "Diagnostic unavailable"),
            ("ab   c", "ab c"),
        ];
        for (input, expected) in table {
            assert_eq!(bounded_persisted_error(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn a_structured_payload_is_omitted_rather_than_truncated_into_storage() {
        assert_eq!(
            bounded_persisted_error(r#"{"assignments":[{"id":"a"}]}"#),
            "Structured diagnostic omitted"
        );
        assert_eq!(bounded_persisted_error("[1,2,3]"), "Structured diagnostic omitted");
    }

    #[test]
    fn an_empty_or_control_only_diagnostic_still_says_something() {
        assert_eq!(bounded_persisted_error(""), "Diagnostic unavailable");
        assert_eq!(bounded_persisted_error("\u{0}\u{1}\t  "), "Diagnostic unavailable");
    }

    #[test]
    fn control_characters_and_runs_of_whitespace_collapse_to_single_spaces() {
        assert_eq!(bounded_persisted_error("a\u{7}\u{7}b   c"), "a b c");
    }

    #[test]
    fn an_over_long_diagnostic_is_bounded_and_says_so() {
        let out = bounded_persisted_error(&"x".repeat(5_000));
        assert_eq!(out.chars().count(), MAX_PERSISTED_ERROR_LENGTH);
        assert!(out.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn bounding_is_idempotent_so_a_read_repair_never_re_truncates() {
        let once = bounded_persisted_error(&"y".repeat(5_000));
        assert_eq!(bounded_persisted_error(&once), once);
    }

    #[test]
    fn a_multibyte_diagnostic_is_never_sliced_mid_character() {
        let out = bounded_persisted_error(&"é".repeat(5_000));
        assert!(out.ends_with(TRUNCATION_SUFFIX));
        assert_eq!(out.chars().count(), MAX_PERSISTED_ERROR_LENGTH);
    }
}
