//! Fail-closed confirmation for the prompts that block or shed state.
//!
//! Ported from the deleted TypeScript `prompt.ts` and the private
//! tri-state copy `reset.ts` carried because a boolean could not tell
//! "the operator said no" from "nobody could be asked".
//!
//! # The rule
//!
//! A non-interactive caller without `--yes` must never be able to answer "yes"
//! implicitly, and must never hang on a prompt nothing will ever answer. It
//! gets a decision instead — and for `reset`, a REFUSAL rather than a silent
//! decline, because a scripted reset that quietly does nothing is worse than
//! one that fails loudly.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

/// What a confirmation resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// `--yes`, or the operator typed y/yes.
    Confirmed,
    /// The operator was asked and said no.
    Declined,
    /// Nobody could be asked and `--yes` was absent.
    RefusedNonInteractive,
}

/// The two facts a confirmation is decided from.
///
/// Injected so the rule is unit-testable without a TTY.
pub(crate) trait Prompt {
    /// Is stdin an interactive terminal?
    fn interactive(&self) -> bool;
    /// Ask, and return the raw answer.
    fn ask(&self, question: &str) -> Option<String>;
}

/// Decide, from one prompt and the `--yes` flag.
pub(crate) fn decide(question: &str, yes: bool, prompt: &dyn Prompt) -> Confirmation {
    if yes {
        return Confirmation::Confirmed;
    }
    if !prompt.interactive() {
        return Confirmation::RefusedNonInteractive;
    }
    match prompt.ask(question) {
        Some(answer) => {
            let answer = answer.trim().to_lowercase();
            if answer == "y" || answer == "yes" {
                Confirmation::Confirmed
            } else {
                Confirmation::Declined
            }
        }
        // A reader that failed is not an operator saying yes.
        None => Confirmation::Declined,
    }
}

/// Is a caller with these two stream states able to answer a question?
///
/// BOTH must be terminals, and this is the whole of the fix. The question is
/// written to stderr and the answer is read from stdin, so a caller missing
/// either one cannot complete the exchange — but only stdin was ever checked.
/// A caller with a pty on stdin and a redirected stderr (a wrapper, a CI
/// runner, an agent driving a pane, `chief attach x 2> log`) therefore passed
/// the check, had the question written somewhere it could not see, and parked
/// on `read_line` forever with no output explaining why. A hang is the worst
/// possible answer to a confirmation: it is indistinguishable from work.
///
/// Pure and separately named so the rule is testable without owning a tty.
#[must_use]
pub(crate) fn can_be_asked(stdin_is_terminal: bool, stderr_is_terminal: bool) -> bool {
    stdin_is_terminal && stderr_is_terminal
}

/// The real terminal. Questions go to stderr so a piped stdout stays clean.
pub(crate) struct Terminal;

impl Prompt for Terminal {
    fn interactive(&self) -> bool {
        can_be_asked(std::io::stdin().is_terminal(), std::io::stderr().is_terminal())
    }

    fn ask(&self, question: &str) -> Option<String> {
        let mut stderr = std::io::stderr();
        stderr.write_all(question.as_bytes()).ok()?;
        stderr.flush().ok()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok()?;
        Some(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::{decide, Confirmation, Prompt};

    struct FakePrompt {
        interactive: bool,
        answer: Option<&'static str>,
    }

    impl Prompt for FakePrompt {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn ask(&self, _question: &str) -> Option<String> {
            self.answer.map(str::to_string)
        }
    }

    #[test]
    fn yes_short_circuits_without_ever_reading_a_prompt() {
        // `answer: None` would be a Declined if it were consulted.
        let prompt = FakePrompt { interactive: false, answer: None };
        assert_eq!(decide("Start 'acme'?", true, &prompt), Confirmation::Confirmed);
    }

    #[test]
    fn a_non_interactive_caller_without_yes_is_refused_never_defaulted() {
        let prompt = FakePrompt { interactive: false, answer: Some("y") };
        assert_eq!(decide("Start 'acme'?", false, &prompt), Confirmation::RefusedNonInteractive);
    }

    #[test]
    fn only_y_and_yes_confirm_and_case_and_whitespace_do_not_matter() {
        for answer in ["y", "Y", "yes", " YES \n"] {
            let prompt = FakePrompt { interactive: true, answer: Some(answer) };
            assert_eq!(decide("q", false, &prompt), Confirmation::Confirmed, "{answer}");
        }
        for answer in ["", "n", "no", "yep", "sure"] {
            let prompt = FakePrompt { interactive: true, answer: Some(answer) };
            assert_eq!(decide("q", false, &prompt), Confirmation::Declined, "{answer}");
        }
    }

    #[test]
    fn a_reader_that_failed_is_a_decline_not_a_confirmation() {
        let prompt = FakePrompt { interactive: true, answer: None };
        assert_eq!(decide("q", false, &prompt), Confirmation::Declined);
    }

    /// The real `Terminal`'s rule, which had no test at all — and that is
    /// exactly where the hang lived. `decide` was correct and fully tested;
    /// the fake it was tested through simply answered `interactive: true` for
    /// a caller the real one also called interactive but that could never see
    /// the question.
    #[test]
    fn a_caller_that_cannot_see_the_question_is_not_interactive() {
        // Both streams: a human at a terminal. The confirmation stays.
        assert!(super::can_be_asked(true, true));
        // stdin redirected — the classic pipe. Already handled before.
        assert!(!super::can_be_asked(false, true));
        // A pty on stdin but stderr redirected: the shape that HUNG. The
        // question was written into a file or a pipe nobody was reading and
        // `read_line` parked forever, with no prompt on screen to explain it.
        assert!(!super::can_be_asked(true, false));
        // Neither: fully detached.
        assert!(!super::can_be_asked(false, false));
    }

    /// The refusal a scripted caller now gets instead of a hang still names
    /// the escape hatch, and `--yes` still short-circuits before the streams
    /// are consulted at all.
    #[test]
    fn a_script_that_passes_yes_is_never_asked_regardless_of_its_streams() {
        for (stdin, stderr) in [(false, false), (true, false), (false, true)] {
            let prompt =
                FakePrompt { interactive: super::can_be_asked(stdin, stderr), answer: None };
            assert_eq!(decide("Start 'acme'?", true, &prompt), Confirmation::Confirmed);
            assert_eq!(
                decide("Start 'acme'?", false, &prompt),
                Confirmation::RefusedNonInteractive,
                "stdin={stdin} stderr={stderr} must refuse rather than park on a prompt"
            );
        }
    }
}
