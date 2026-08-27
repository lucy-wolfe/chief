//! The `tracing` layer that mirrors events and spans into [`crate::sink`].
//!
//! # What this buys that the console formatter does not
//!
//! `chiefd_launch_company` once ran for 4 minutes 34 seconds behind a bare
//! spinner. Proving where that time went needed SSH, `/proc`, `ss`, a pane
//! capture and `strings` on the release binary — and still failed, because the
//! console formatter writes to a stdout nobody was keeping. A file the process
//! appends to as it goes answers the same question by itself.
//!
//! # The two line shapes
//!
//! * **An event** — one `tracing::info!`/`warn!`/… call. Its `event` name is
//!   the value of an explicit `event = "…"` field when the call site states one
//!   (every step instrumented for the launch path does), and the message
//!   otherwise. The message always lands in `detail.message`, so a call site
//!   that states neither is still readable.
//! * **A span** — two lines, `phase: "enter"` when it opens and
//!   `phase: "exit"` when it closes, the second carrying `detail.durationMs`.
//!   That pair is the whole point: a step that can take minutes and logs
//!   nothing is the defect this exists to delete.
//!
//! # Which company a line is about
//!
//! The sink is constructed org-agnostic, because the process that owns the slow
//! part of a launch does not have a company yet. A line names one anyway when
//! its own fields do (`company`, `organization` or `slug`), or when any span
//! open around it does — so `daemon.start`'s retry lines inherit the slug from
//! the `genesis.launch` span that encloses them without every call site
//! repeating it.
//!
//! # Which GESTURE a line is about
//!
//! The same mechanism, for the second question an operator asks of this file:
//! *which click caused this?* A `detail.gesture_id` set on a span reaches every
//! line emitted inside it, at any depth, without the call sites knowing they are
//! inside a gesture. That is what lets one click's whole funnel —
//! `sidebar.click`, the `effects` verbs it runs, the frame it paints, the wake
//! it posts and the answer that comes back on another task — be selected with
//! one `jq`, in a file where three thousand lines share one `session` value and
//! the rail's pid is replaced mid-episode.
//!
//! It stays inside `detail` deliberately. The top-level key set is closed and
//! mirrored in TypeScript (`sink::TOP_LEVEL_KEYS`), and a correlator that
//! forced a schema change on both sides would be a far larger thing than the
//! question it answers.
//!
//! # Secrets
//!
//! Every string that reaches a line — event name, message and each field value
//! — passes [`host_primitives::redact::redact`], the same mask both actuators
//! already apply to host diagnostics. A log is the last place a credential can
//! escape, so it uses that mask rather than a second opinion about what a
//! credential looks like.

use std::collections::BTreeMap;
use std::time::Instant;

use host_primitives::redact::redact;
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::sink::OrgLog;

/// The field a call site sets to name its event. Absent, the message is used.
const EVENT_FIELD: &str = "event";

/// The field names a line's company is read from, in order.
///
/// Three spellings because three already exist in this workspace and renaming
/// them is a product change: `company` is what the operator client calls it,
/// `organization` is the log schema's own key, and `slug` is what the daemon's
/// arguments call it.
const COMPANY_FIELDS: [&str; 3] = ["company", "organization", "slug"];

/// The field that names the operator gesture a line belongs to.
///
/// ONE spelling, and it is the one the rail stamps at the mouse event
/// (`chief_cli::sidebar::gesture`). A second spelling would split a funnel in
/// half, which is the exact failure this field exists to end.
const GESTURE_FIELD: &str = "gesture_id";

/// `tracing`'s own key for the text of a `info!("…")` call.
const MESSAGE_FIELD: &str = "message";

/// What one open span remembers until it closes.
struct SpanRecord {
    /// When the span opened. The only reason a span costs anything here.
    opened: Instant,
    /// The span's own fields, already redacted, inherited by its exit line.
    fields: Map<String, Value>,
    /// The company this span is about, its own or its parent's.
    company: Option<String>,
    /// The gesture this span is inside, its own or its parent's.
    gesture: Option<Value>,
}

/// A [`Layer`] that appends every event and span to one [`OrgLog`] stream.
pub struct SinkLayer {
    sink: OrgLog,
}

impl SinkLayer {
    /// A layer writing this service's stream, resolved from the environment,
    /// or `None` when nothing names a directory to write into.
    ///
    /// `service` names the file: `<root>/<service>.jsonl`. Use the program name
    /// (`chief`, `chiefd`, `beacond`) so one directory listing reads as a list
    /// of the programs that have run.
    #[must_use]
    pub fn from_env(service: &str) -> Option<Self> {
        OrgLog::from_env(service, None).map(|sink| Self { sink })
    }

    /// A layer writing an explicit sink. Tests point this at a throwaway
    /// directory; nothing in production does.
    #[must_use]
    pub const fn new(sink: OrgLog) -> Self {
        Self { sink }
    }
}

/// The level name a line carries, lowercase to match the schema.
const fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "trace",
        Level::DEBUG => "debug",
        Level::INFO => "info",
        Level::WARN => "warn",
        Level::ERROR => "error",
    }
}

/// Collects `tracing` fields into a JSON object, redacting every string.
///
/// A `BTreeMap` rather than the target `Map` so the keys come out ordered: two
/// lines about the same step must be diffable, and `serde_json`'s default map
/// preserves insertion order, which is the order the macro happened to use.
#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, Value>,
}

impl FieldVisitor {
    /// The collected fields, in key order.
    fn into_map(self) -> Map<String, Value> {
        self.fields.into_iter().collect()
    }

    fn insert(&mut self, field: &Field, value: Value) {
        self.fields.insert(field.name().to_owned(), value);
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::String(redact(value)));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, Value::String(redact(&format!("{value:?}"))));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, Value::String(redact(&value.to_string())));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::Bool(value));
    }
}

/// The company a field set names, if any.
fn company_of(fields: &Map<String, Value>) -> Option<String> {
    COMPANY_FIELDS
        .iter()
        .find_map(|name| fields.get(*name))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

/// The event name a field set states, if any. Removed from the fields, because
/// it becomes the line's top-level `event` and repeating it in `detail` would
/// make one fact two.
fn take_event_name(fields: &mut Map<String, Value>) -> Option<String> {
    fields.remove(EVENT_FIELD).as_ref().and_then(Value::as_str).map(str::to_owned)
}

/// The message a field set carries, left in place so a line that used it as its
/// name still shows the full text.
fn message_of(fields: &Map<String, Value>) -> Option<String> {
    fields.get(MESSAGE_FIELD).and_then(Value::as_str).map(str::to_owned)
}

/// The gesture a field set names, if any.
///
/// A JSON `null` is treated as absent so that
/// `tracing::info_span!("…", gesture_id = tracing::field::Empty)` — the shape a
/// caller uses when the id arrives late — does not shadow an enclosing span's
/// answer with nothing.
fn gesture_of(fields: &Map<String, Value>) -> Option<Value> {
    fields.get(GESTURE_FIELD).filter(|value| !value.is_null()).cloned()
}

impl<S> Layer<S> for SinkLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        attributes.record(&mut visitor);
        let mut fields = visitor.into_map();
        // The enclosing span's company, when this one does not name its own.
        // Walking the parents is what lets `daemon.start`'s retry lines say
        // which company they are stalling on without the loop being told.
        let company = company_of(&fields).or_else(|| {
            ctx.span_scope(id)?.skip(1).find_map(|span| {
                span.extensions().get::<SpanRecord>().and_then(|record| record.company.clone())
            })
        });
        // The same walk for the gesture, and it is WRITTEN BACK into the
        // span's own fields rather than kept beside them: a nested span's
        // enter and exit lines then carry the id too, and every event inside
        // this span finds the answer at the first record it looks at instead
        // of walking to the root for it.
        let gesture = gesture_of(&fields).or_else(|| {
            ctx.span_scope(id)?.skip(1).find_map(|span| {
                span.extensions().get::<SpanRecord>().and_then(|record| record.gesture.clone())
            })
        });
        if let Some(gesture) = gesture.clone() {
            fields.insert(GESTURE_FIELD.to_owned(), gesture);
        }

        let name = attributes.metadata().name();
        let mut detail = fields.clone();
        detail.insert("phase".to_owned(), Value::from("enter"));
        detail.insert("target".to_owned(), Value::from(attributes.metadata().target()));
        self.sink.emit_object_for(
            company.as_deref(),
            level_name(attributes.metadata().level()),
            name,
            &detail,
        );

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanRecord {
                opened: Instant::now(),
                fields,
                company,
                gesture,
            });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        let recorded = visitor.into_map();
        let mut extensions = span.extensions_mut();
        if let Some(record) = extensions.get_mut::<SpanRecord>() {
            if record.company.is_none() {
                record.company = company_of(&recorded);
            }
            if record.gesture.is_none() {
                record.gesture = gesture_of(&recorded);
            }
            record.fields.extend(recorded);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let mut fields = visitor.into_map();

        let company = company_of(&fields).or_else(|| {
            ctx.event_scope(event)?.find_map(|span| {
                span.extensions().get::<SpanRecord>().and_then(|record| record.company.clone())
            })
        });
        // WHICH CLICK CAUSED THIS LINE. A call site inside a gesture states
        // nothing; the span the gesture opened answers for all of them. An
        // event that names its own id keeps it — a line about a DIFFERENT
        // gesture than the one it runs inside is a real case (a wake answered
        // long after the click that asked for it) and must be able to say so.
        if !fields.contains_key(GESTURE_FIELD) {
            if let Some(gesture) = ctx.event_scope(event).and_then(|scope| {
                scope.into_iter().find_map(|span| {
                    span.extensions().get::<SpanRecord>().and_then(|record| record.gesture.clone())
                })
            }) {
                fields.insert(GESTURE_FIELD.to_owned(), gesture);
            }
        }

        // An explicit `event = "…"` is the stable, greppable name. A call site
        // that states none is named by its message, which is what the ~150
        // `tracing` calls that predate this layer carry.
        let stated = take_event_name(&mut fields);
        let name = stated
            .or_else(|| message_of(&fields))
            .unwrap_or_else(|| event.metadata().target().to_owned());

        fields.insert("target".to_owned(), Value::from(event.metadata().target()));
        self.sink.emit_object_for(
            company.as_deref(),
            level_name(event.metadata().level()),
            &redact(&name),
            &fields,
        );
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();
        let Some(record) = extensions.get::<SpanRecord>() else { return };

        let mut detail = record.fields.clone();
        detail.insert("phase".to_owned(), Value::from("exit"));
        detail.insert("target".to_owned(), Value::from(span.metadata().target()));
        // THE measurement. A step that blocks for minutes now says so on the
        // line that ends it, in one field, without anybody subtracting
        // timestamps by hand.
        detail.insert(
            "durationMs".to_owned(),
            Value::from(u64::try_from(record.opened.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        self.sink.emit_object_for(
            record.company.as_deref(),
            level_name(span.metadata().level()),
            span.metadata().name(),
            &detail,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::SinkLayer;
    use crate::sink::OrgLog;
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use tracing_subscriber::layer::SubscriberExt as _;

    /// Run `body` with a subscriber whose only layer is the sink, and read back
    /// every line it wrote.
    ///
    /// `with_default` rather than a global install: these tests run beside each
    /// other in one process, and a global subscriber can be set exactly once.
    ///
    /// The subscriber is thread-local; the CALLSITE CACHE it reads is not, and
    /// that is what [`permit_every_callsite`] exists for. Nothing here works
    /// without it — see its own comment.
    fn recorded(name: &str, body: impl FnOnce()) -> Vec<Value> {
        permit_every_callsite();

        let dir = tempdir(name);
        let sink = OrgLog::new(&dir, "chiefd", crate::sink::NO_ORGANIZATION);
        let path = sink.path().to_path_buf();
        let subscriber = tracing_subscriber::registry().with(SinkLayer::new(sink));
        tracing::subscriber::with_default(subscriber, body);
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn tempdir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("chiefd-log-layer-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Install one permissive process-wide subscriber, so that no `tracing`
    /// callsite in this test binary can ever be cached as "nobody is
    /// interested".
    ///
    /// # The race this closes
    ///
    /// `with_default` is thread-local, so one test's lines cannot reach
    /// another's recorder. True — and beside the point, because whether a
    /// callsite emits AT ALL is decided once, process-globally, the first time
    /// that line of code executes, and is then cached in a static for the life
    /// of the process (`tracing_core::callsite`). `tracing-core` computes that
    /// decision from every LIVE dispatcher — except when exactly one is alive,
    /// where it takes the shortcut of asking only the REGISTERING THREAD's
    /// default subscriber.
    ///
    /// A test binary hits that shortcut constantly. While one test holds the
    /// only live dispatcher (its own scoped recorder), every other test thread
    /// has no subscriber at all, and `NoSubscriber` answers `Interest::never`.
    /// Whichever thread reaches one of these tests' `tracing` lines first
    /// therefore decides whether that line can ever be recorded again, and a
    /// test that lost the race read an EMPTY file — a failure that reproduces
    /// only under parallelism and passes every time in isolation. It was
    /// diagnosed and fixed on the operator client's two recorders first; the
    /// same comment stands over the same fix in all three places, because the
    /// recorders belong to different targets and cannot share one helper.
    ///
    /// A permanently-installed global default fixes both halves. It is a
    /// dispatcher that is always alive, so the one-dispatcher shortcut is never
    /// taken while a recorder is running and the decision is the union over the
    /// recorder too; and when it IS the only one, it is a bare `Registry`,
    /// whose answer is `Interest::always` rather than `never`. It has no
    /// layers, so it records nothing and cannot pollute a recorder — the scoped
    /// subscriber still wins on the thread that installs it.
    ///
    /// Idempotent: `set_global_default` succeeds once per process and every
    /// later call is a no-op.
    fn permit_every_callsite() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
    }

    /// One `tracing` callsite two threads can reach.
    ///
    /// A `debug!` written inline in each place would be two DIFFERENT
    /// callsites with independent caches, and the test below would prove
    /// nothing. This is the shared line.
    fn shared_callsite() {
        tracing::debug!(event = "test.shared.callsite", "one callsite, two threads");
    }

    /// The RULE every recorder in this crate rests on: what a test records is
    /// decided by the subscriber that test installed, and no thread running
    /// beside it can switch the line off.
    ///
    /// The subscriber-less thread reaches the callsite FIRST, which is the
    /// whole race. Without [`permit_every_callsite`] the spawned thread caches
    /// `Interest::never` for the process and this fails every single run.
    #[test]
    fn a_recording_survives_a_subscriberless_thread_reaching_the_same_callsite() {
        let lines = recorded("callsite-cache", || {
            std::thread::spawn(shared_callsite).join().expect("the racing thread must finish");
            shared_callsite();
        });

        assert!(
            lines.iter().any(|line| line["event"] == "test.shared.callsite"),
            "a thread with no subscriber must not be able to silence a recorder: {lines:?}"
        );
    }

    /// Whether the calling thread, having installed no subscriber of its own,
    /// falls through to `NoSubscriber` — the answer that returns
    /// `Interest::never` and caches it for the first callsite this thread
    /// reaches.
    fn falls_through_to_no_subscriber() -> bool {
        tracing::dispatcher::get_default(|d| d.is::<tracing::subscriber::NoSubscriber>())
    }

    /// The same rule, stated so that it cannot pass by luck.
    ///
    /// The test above is the shape both `chief-cli` recorders carry, and it is
    /// kept identical to them on purpose. It is not, on its own, a sufficient
    /// regression test IN THIS BINARY, and the reason is worth writing down:
    /// `has_just_one` is only true while exactly one dispatcher is LIVE, and
    /// every test in this module holds a recorder, so a second recorder running
    /// beside this one sends the decision down the union path
    /// (`Interest::and` keeps the MORE permissive answer) and rescues the
    /// callsite. Verified, not assumed: with the fix removed, CI ran the test
    /// above green.
    ///
    /// This one has no such dependence on what else is running. It states the
    /// precondition the whole mechanism rests on — after any recorder has been
    /// built, NO thread in this process is left with `NoSubscriber` as its
    /// default — and `NoSubscriber::register_callsite` answering
    /// `Interest::never` is the only way a callsite is ever switched off for
    /// good. Remove [`permit_every_callsite`] and this fails on every run, on
    /// every machine, whatever order the tests take.
    #[test]
    fn a_thread_with_no_subscriber_of_its_own_still_sees_a_permissive_process_default() {
        // Builds a recorder and nothing else; what is under test is the state
        // that building one leaves the PROCESS in.
        let _ = recorded("permissive-default", || {});

        let racing = std::thread::spawn(falls_through_to_no_subscriber);
        let subscriberless = racing.join().expect("the racing thread must finish");

        assert!(
            !subscriberless,
            "a thread with no subscriber of its own must fall back to the permissive process \
             default; a `NoSubscriber` answers `Interest::never` and caches it for every callsite \
             it reaches first"
        );
    }

    /// Occupy the calling thread for `millis` of REAL wall-clock time.
    ///
    /// Not `std::thread::sleep`, and not because the ban is inconvenient: the
    /// ban exists so production waiting flows through the injected `Clock`, and
    /// a `Clock` is exactly what cannot help here. `durationMs` is measured
    /// from a real `Instant` inside the layer, so the only way to prove the
    /// measurement is real — rather than a constant zero that would satisfy a
    /// weaker assertion — is for real time to pass. Spinning says that plainly
    /// and needs no exemption.
    fn occupy(millis: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(millis);
        while std::time::Instant::now() < until {
            std::hint::spin_loop();
        }
    }

    /// The defect being fixed, stated as a test: a step that blocks must leave
    /// a line saying how long it blocked for.
    #[test]
    fn a_span_records_how_long_it_took_on_the_line_that_closes_it() {
        let lines = recorded("duration", || {
            let span = tracing::info_span!("daemon.start", company = "acme");
            let entered = span.enter();
            occupy(20);
            drop(entered);
            drop(span);
        });

        assert_eq!(lines.len(), 2, "a span writes an enter line and an exit line");
        assert_eq!(lines[0]["event"], "daemon.start");
        assert_eq!(lines[0]["detail"]["phase"], "enter");
        assert!(lines[0]["detail"].get("durationMs").is_none(), "an open span has no duration yet");

        assert_eq!(lines[1]["event"], "daemon.start");
        assert_eq!(lines[1]["detail"]["phase"], "exit");
        let elapsed = lines[1]["detail"]["durationMs"].as_u64().unwrap();
        assert!(elapsed >= 20, "the exit line must carry the real elapsed time, got {elapsed}");
        // Both halves name the company, so one grep finds the whole step.
        assert_eq!(lines[0]["organization"], "acme");
        assert_eq!(lines[1]["organization"], "acme");
    }

    /// The retry loops are inside the launch span and do not repeat the slug.
    /// A line that could not say which company it was about would be useless on
    /// a box running several.
    #[test]
    fn an_event_inherits_the_company_from_the_span_around_it() {
        let lines = recorded("inherit", || {
            let span = tracing::info_span!("genesis.launch", company = "northwind");
            let _entered = span.enter();
            let inner = tracing::info_span!("daemon.start");
            let _inner = inner.enter();
            tracing::info!(event = "daemon.registration.wait", attempt = 3_u64, "still waiting");
        });

        let waited = lines
            .iter()
            .find(|line| line["event"] == "daemon.registration.wait")
            .expect("the attempt line");
        assert_eq!(waited["organization"], "northwind");
        assert_eq!(waited["detail"]["attempt"], 3);
        assert_eq!(waited["detail"]["message"], "still waiting");
        // The nested span inherited it too, so its own duration line is
        // attributable without re-stating the slug.
        let nested = lines.iter().find(|line| line["event"] == "daemon.start").expect("the span");
        assert_eq!(nested["organization"], "northwind");
    }

    /// A call site that states no `event` is still readable: the message names
    /// it. This is what the `tracing` calls that predate the layer rely on.
    #[test]
    fn an_event_with_no_stated_name_is_named_by_its_message() {
        let lines = recorded("message-name", || {
            tracing::warn!(path = "/tmp/x", "the root provider registry could not be read");
        });
        assert_eq!(lines[0]["event"], "the root provider registry could not be read");
        assert_eq!(lines[0]["level"], "warn");
        assert_eq!(lines[0]["detail"]["path"], "/tmp/x");
    }

    /// A stated `event` becomes the name and is NOT repeated inside `detail` —
    /// one fact, one place.
    #[test]
    fn a_stated_event_name_wins_and_is_not_duplicated_into_the_detail() {
        let lines = recorded("stated-name", || {
            tracing::info!(event = "company.create.request", "creating");
        });
        assert_eq!(lines[0]["event"], "company.create.request");
        assert!(lines[0]["detail"].get("event").is_none());
        assert_eq!(lines[0]["detail"]["message"], "creating");
    }

    /// The environment carries `OPENROUTER_API_KEY`, `XCOM_API_KEY` and
    /// `TRIBES_SSH_PUBLIC_KEY`. A log is the last place any of them can escape.
    #[test]
    fn credential_shaped_values_never_reach_the_file() {
        let lines = recorded("redaction", || {
            tracing::error!(
                detail = "spawn failed: OPENROUTER_API_KEY=sk-live-abc123 XCOM_API_KEY=xk-9",
                "provider probe failed with sk-live-abc123"
            );
        });
        let rendered = serde_json::to_string(&lines[0]).unwrap();
        assert!(!rendered.contains("sk-live-abc123"), "a credential reached the log: {rendered}");
        assert!(rendered.contains("[redacted]"), "the mask must be visible: {rendered}");
        // The surrounding diagnostic survives, because operators read these.
        assert!(rendered.contains("spawn failed"));
    }

    /// Span fields recorded after the span opened still reach its exit line.
    #[test]
    fn a_field_recorded_after_the_span_opened_reaches_its_exit_line() {
        let lines = recorded("record", || {
            let span = tracing::info_span!("company.create", company = tracing::field::Empty);
            span.record("company", "late-slug");
            drop(span);
        });
        let exit = lines.iter().find(|line| line["detail"]["phase"] == "exit").expect("exit line");
        assert_eq!(exit["organization"], "late-slug");
    }

    /// THE RULE STAGE 0 RESTS ON: a line emitted inside a gesture names that
    /// gesture, whether or not the call site knows there is one.
    ///
    /// Without this, a `gesture_id` would have to be restated at each of the
    /// ~62 `sidebar/` call sites and the ~13 in `actuate/`, and the one that
    /// was forgotten would be the one an operator needed.
    #[test]
    fn an_event_inherits_the_gesture_from_the_span_around_it() {
        let lines = recorded("gesture-inherit", || {
            let span =
                tracing::info_span!("sidebar.gesture", gesture_id = 1_755_000_000_123_456_u64);
            let _entered = span.enter();
            tracing::info!(event = "sidebar.person.moved", person = "dev", "moved a person");
        });

        let moved = lines
            .iter()
            .find(|line| line["event"] == "sidebar.person.moved")
            .expect("the effect line");
        assert_eq!(
            moved["detail"]["gesture_id"], 1_755_000_000_123_456_u64,
            "an effect the click ran must carry the click's own id: {lines:?}"
        );
    }

    /// The funnel crosses spans, so the id must survive nesting — and the
    /// nested span's OWN two lines must carry it, because the duration they
    /// measure is part of the gesture's cost.
    #[test]
    fn a_nested_span_and_its_events_both_carry_the_enclosing_gesture() {
        let lines = recorded("gesture-nested", || {
            let outer = tracing::info_span!("sidebar.gesture", gesture_id = 77_u64);
            let _outer = outer.enter();
            let inner = tracing::info_span!("sidebar.effects.show-person");
            let _inner = inner.enter();
            tracing::info!(event = "sidebar.window.laid", "laid the window out");
        });

        for line in &lines {
            assert_eq!(
                line["detail"]["gesture_id"], 77,
                "every line inside the gesture must name it: {line:?}"
            );
        }
    }

    /// A line that names its OWN gesture keeps it. The wake answer is the
    /// case: it is emitted on a task the click spawned, long after the click,
    /// and it must be able to say which click it is answering even if the
    /// process has since opened another gesture around it.
    #[test]
    fn an_event_that_names_its_own_gesture_is_not_overwritten_by_the_span() {
        let lines = recorded("gesture-own", || {
            let span = tracing::info_span!("sidebar.gesture", gesture_id = 2_u64);
            let _entered = span.enter();
            tracing::info!(event = "sidebar.wake.answered", gesture_id = 1_u64, "an older wake");
        });

        let answered = lines
            .iter()
            .find(|line| line["event"] == "sidebar.wake.answered")
            .expect("the answer line");
        assert_eq!(answered["detail"]["gesture_id"], 1, "the call site's own id wins: {lines:?}");
    }

    /// A line outside any gesture says nothing about one. An absent
    /// correlator must stay absent rather than become a zero, or "no gesture"
    /// and "gesture 0" become the same fact.
    #[test]
    fn a_line_outside_any_gesture_carries_no_gesture_id() {
        let lines = recorded("gesture-absent", || {
            tracing::info!(event = "actuator.round", round = 4_u64, "a converge pass finished");
        });
        assert!(
            lines[0]["detail"].get("gesture_id").is_none(),
            "a background line must not claim a gesture: {lines:?}"
        );
    }

    /// A line about no company still says so explicitly, so the presence of a
    /// real slug stays meaningful.
    #[test]
    fn a_line_about_no_company_names_the_absence_rather_than_omitting_it() {
        let lines = recorded("no-company", || tracing::info!(event = "cli.start", "chief ls"));
        assert_eq!(lines[0]["organization"], crate::sink::NO_ORGANIZATION);
    }

    /// The stream a `chiefd` process writes is the one the incident report
    /// said did not exist.
    #[test]
    fn the_service_name_selects_the_file() {
        let dir = tempdir("service-file");
        let sink = OrgLog::new(&dir, "chiefd", crate::sink::NO_ORGANIZATION);
        assert_eq!(sink.path(), Path::new(&dir).join("chiefd.jsonl"));
    }
}
