// An integration test is its own crate, so `clippy.toml`'s
// `allow-expect-in-tests` does not reach these helpers. A failed `expect` here
// is the runner failing, which is the intended outcome.
#![allow(clippy::expect_used, clippy::panic, dead_code)]

//! Shared machinery for the conformance runners.
//!
//! `conformance/README.md` describes `run-rs.rs` as the Rust half of the golden
//! corpus. This module is the part the replaying families (`activity`,
//! `session-maintenance`) share: fixture loading and shape
//! checks, the deterministic key-sorted comparison, and the [`World`] every
//! fixture runs in.
//!
//! # Why a `World` and not a database
//!
//! The corpus is a *store-layer* corpus: it records what the durable stores do,
//! and TESTING.md §3 puts the actor and the host executor behind their own
//! tests. So a fixture replays against an in-memory [`Ledgers`], which is the
//! exact working set the writer actor commits. The one thing that would be lost
//! — that a commit really is one transaction — is asserted separately and much
//! more strongly by `tests/inv14_single_commit.rs`, which drives a real
//! `CompanyDb` and reads back through a second connection.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chiefd_core::clock::WallMillis;
use chiefd_core::isotime::iso_millis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::store::organization::{self, OrganizationManifest};
use chiefd_core::store::session_maintenance::SessionMaintenanceLedger;
use chiefd_core::store::{activity, supervision};
use chiefd_core::test_support::northstar_manifest;
use chiefd_core::ChiefdError;
use serde_json::{json, Map, Value};

/// The frozen epoch every fixture starts at, re-exported from its one
/// definition. It was a copy here citing `conformance/lib/world.ts`, which is
/// deleted; the constant now lives in `chiefd_core::test_support`, which every
/// conformance runner in all three crates can already read.
pub use chiefd_core::test_support::CONFORMANCE_EPOCH;

/// The `northstar` template's derived facts, as the fixtures were recorded.
pub const NORTHSTAR_SLUG: &str = "northstar-conformance";
/// People, in manifest order.
pub const NORTHSTAR_PEOPLE: [&str; 4] = ["chief", "quant-head", "signal-researcher", "it-head"];
/// Departments, sorted — the shape `company.create` projects.
pub const NORTHSTAR_DEPARTMENTS: [&str; 3] = ["executive", "it", "quant"];

/// The deterministic world one fixture runs in.
pub struct World {
    /// The working set the writer actor would commit.
    pub ledgers: Ledgers,
    /// The manifest, once `company.create` has run.
    pub manifest: Option<OrganizationManifest>,
    /// The session-maintenance ledger, once `company.create` has run.
    ///
    /// It sits beside [`Self::ledgers`] rather than inside it because the
    /// session-maintenance verbs
    /// ([`chiefd_core::store::session_maintenance_ops`]) are total functions of
    /// `(&mut SessionMaintenanceLedger, …, at)` — exactly the values the writer
    /// actor reads inside its one transaction — while the activity and
    /// supervision verbs take the whole working set. Modelling each family the
    /// way its own store takes it is what keeps the runner a replay of
    /// production code rather than a re-implementation of it.
    pub maintenance: Option<SessionMaintenanceLedger>,
    /// Whether the session-maintenance document has ever been written.
    ///
    /// The corpus distinguishes an **absent** ledger from an **empty** one:
    /// `maint.summary` records the literal `null` on a company where no verb
    /// has ever changed anything, and `{requestOrder: [], …}` on one where a
    /// verb ran and produced nothing. At the store layer that boundary is
    /// exactly "a verb mutated the ledger", which is what
    /// [`Self::observe_maintenance_write`] records.
    pub maintenance_written: bool,
}

impl World {
    /// An empty world at the frozen epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ledgers: Ledgers::empty(WallMillis(CONFORMANCE_EPOCH)),
            manifest: None,
            maintenance: None,
            maintenance_written: false,
        }
    }

    /// Record whether a verb just changed the session-maintenance ledger.
    ///
    /// `before` is the ledger as it stood when the verb was entered, and is
    /// `None` for `company.create` itself — creating the company does not write
    /// the maintenance document. A verb that refused, or that succeeded without
    /// producing anything (a bounded idle probe), likewise leaves it unwritten,
    /// which is the state the corpus records as `null`.
    pub fn observe_maintenance_write(&mut self, before: Option<&SessionMaintenanceLedger>) {
        if before.is_some() && self.maintenance.as_ref() != before {
            self.maintenance_written = true;
        }
    }

    /// The manifest, or a panic naming the fixture bug.
    #[must_use]
    pub fn manifest(&self) -> &OrganizationManifest {
        self.manifest.as_ref().expect("fixture used a company op before `company.create`")
    }

    /// The session-maintenance ledger, or a panic naming the fixture bug.
    #[must_use]
    pub fn maintenance(&self) -> &SessionMaintenanceLedger {
        self.maintenance.as_ref().expect("fixture used a maint op before `company.create`")
    }

    /// The session-maintenance ledger, mutably.
    pub fn maintenance_mut(&mut self) -> &mut SessionMaintenanceLedger {
        self.maintenance.as_mut().expect("fixture used a maint op before `company.create`")
    }

    /// The frozen clock as an ISO instant — the `at` every store verb takes.
    #[must_use]
    pub fn now_iso(&self) -> String {
        iso_millis(self.ledgers.now().0)
    }

    /// The supervision ledger as it stands.
    #[must_use]
    pub fn supervision(&self) -> supervision::SupervisionLedger {
        supervision::read(&self.ledgers, self.manifest())
            .expect("the supervision ledger is readable")
    }

    /// Move the frozen clock forward.
    pub fn advance(&mut self, millis: i64) {
        assert!(millis >= 0, "clock.advance requires a non-negative integer");
        let now = self.ledgers.now().0 + millis;
        self.ledgers.set_now_for_test(WallMillis(now));
    }

    /// `company.create` — build the manifest and seed the dependent ledgers.
    ///
    /// This is the port of `createOrganization`'s durable half: the manifest,
    /// then the supervision ledger, then the activity ledger (whose seed
    /// asserts inv 20: `lastDesiredActive:false`).
    pub fn create_company(&mut self, template: &str) -> Value {
        assert_eq!(template, "northstar", "only one template is modelled");
        assert!(self.manifest.is_none(), "a fixture may create only one company");
        let manifest = northstar_manifest(CONFORMANCE_EPOCH);
        organization::create(&mut self.ledgers, &manifest).expect("the template manifest is valid");
        supervision::seed(&mut self.ledgers, &manifest).expect("supervision seeds");
        activity::seed(&mut self.ledgers, &manifest).expect("activity seeds");
        // The session-maintenance ledger is created empty at the same instant,
        // which is what `createOrganization` left behind and what every
        // `maint.*` fixture's first read expects.
        self.maintenance =
            Some(SessionMaintenanceLedger::initial(NORTHSTAR_SLUG, &iso_millis(CONFORMANCE_EPOCH)));
        self.manifest = Some(manifest);
        json!({
            "slug": NORTHSTAR_SLUG,
            "people": NORTHSTAR_PEOPLE,
            "departments": NORTHSTAR_DEPARTMENTS,
        })
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// A fixture's `expect`, normalized.
pub enum Expectation {
    /// A bounded projection.
    Ok(Value),
    /// A taxonomy error, matched on `{type, code}` only.
    Error {
        /// The wire discriminant.
        kind: String,
        /// The machine code.
        code: String,
    },
}

/// The repo's `conformance/fixtures/<family>` directory.
///
/// `CARGO_MANIFEST_DIR` is baked into this test binary at COMPILE time
/// (#1002): under a shared, persistent `CARGO_TARGET_DIR` a cached binary can
/// outlive the checkout it was built from, so the baked path can point at a
/// directory that no longer exists. There is no reliable way to rediscover
/// the real checkout at runtime (the binary's own location, under the shared
/// target dir, carries no such information) — so this fails LOUDLY and
/// specifically instead of surfacing as a bare "file not found" from
/// whatever first tries to read a fixture out of a dead directory.
#[must_use]
pub fn fixture_dir(family: &str) -> PathBuf {
    repo_root().join("conformance/fixtures").join(family)
}

/// The repo root, with the #1002 staleness check described on [`fixture_dir`].
#[must_use]
pub fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        manifest.is_dir(),
        "this test binary was compiled with CARGO_MANIFEST_DIR={} baked in at compile time, \
         but that directory no longer exists on this host. This is the #1002 shape: a shared \
         CARGO_TARGET_DIR served a binary built from a checkout that has since been deleted. \
         Fix: `cargo clean -p chiefd-core` (or the owning crate) and rebuild from a live checkout.",
        manifest.display()
    );
    manifest
        .ancestors()
        .nth(4)
        .expect("the repo root is four levels above this crate")
        .to_path_buf()
}

/// Load every fixture of a family, with the FORMAT.md shape checks.
#[must_use]
pub fn load_fixtures(family: &str) -> Vec<(String, Value)> {
    let dir = fixture_dir(family);
    let mut fixtures: Vec<(String, Value)> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()));
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("a fixture file has a name")
                .to_string();
            // FORMAT.md: `family`/`name` must agree with the directory and the
            // filename, so a fixture cannot be moved or renamed silently.
            assert_eq!(value["family"], json!(family), "{stem}: wrong family");
            assert_eq!(value["name"], json!(stem.clone()), "{stem}: name/filename disagree");
            assert!(
                value["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{stem}: a fixture must say what it pins"
            );
            (stem, value)
        })
        .collect();
    fixtures.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!fixtures.is_empty(), "found no fixtures — the runner would pass vacuously");
    fixtures
}

/// Recursively sort object keys so a comparison failure shows a behaviour
/// difference rather than an ordering one (FORMAT.md determinism rule 6).
#[must_use]
pub fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                out.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Normalize a fixture's `expect` block.
#[must_use]
pub fn expectation(fixture: &Value) -> Expectation {
    let expect = &fixture["expect"];
    if let Some(error) = expect.get("error") {
        return Expectation::Error {
            kind: error["type"].as_str().expect("an error type").to_string(),
            code: error["code"].as_str().expect("an error code").to_string(),
        };
    }
    Expectation::Ok(sorted(expect.get("ok").expect("`expect` is `ok` or `error`")))
}

/// The `(type, code)` pair the corpus matches on.
#[must_use]
pub fn taxonomy(error: &ChiefdError) -> (String, String) {
    (
        error.kind().to_string(),
        error.code().map_or_else(|| "unclassified".to_string(), ToString::to_string),
    )
}

/// A required string input.
#[must_use]
pub fn text(input: &Value, key: &str) -> String {
    input[key]
        .as_str()
        .unwrap_or_else(|| panic!("fixture input '{key}' must be a non-empty string"))
        .to_string()
}

/// An optional string input.
#[must_use]
pub fn optional_text(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(ToString::to_string)
}

/// A required integer input.
#[must_use]
pub fn integer(input: &Value, key: &str) -> i64 {
    input[key].as_i64().unwrap_or_else(|| panic!("fixture input '{key}' must be an integer"))
}

/// An optional string-array input; absent is an empty list.
#[must_use]
pub fn string_list(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .unwrap_or_else(|| panic!("fixture input '{key}' must be a string array"))
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The authenticated caller a worker-issued op presents.
#[must_use]
pub fn caller_person(caller: Option<&Value>) -> String {
    let caller = caller.expect("this op requires a fixture `caller`");
    caller["personId"].as_str().expect("a caller personId").to_string()
}

/// Run one fixture's `setup` steps, panicking loudly on any refusal.
///
/// FORMAT.md: every setup step must succeed. A fixture that cannot reach its
/// precondition is broken, not refused.
pub fn run_setup(
    world: &mut World,
    name: &str,
    fixture: &Value,
    mut run: impl FnMut(&mut World, &str, &Value, Option<&Value>) -> Result<Value, (String, String)>,
) {
    for (index, step) in
        fixture["setup"].as_array().expect("`setup` is an array").iter().enumerate()
    {
        let op = step["op"].as_str().expect("a setup op");
        let input = step.get("in").cloned().unwrap_or_else(|| json!({}));
        run(world, op, &input, step.get("caller")).unwrap_or_else(|(kind, code)| {
            panic!("{name}: setup step {index} ({op}) failed with {kind}/{code}")
        });
    }
}

/// The model catalog is not ported (see `test_support::TEMPLATE_MODEL`).
///
/// This asserts the placeholder cannot become load-bearing: no fixture of
/// `family` may mention a person's model, provider or task class. If one ever
/// does, the template has to derive them for real rather than stand in.
pub fn assert_no_fixture_depends_on_the_model_catalog(family: &str) {
    assert_no_fixture_observes(family, &["model", "provider", "taskClass", "modelReason"]);
}

/// No fixture of `family` may observe any of `keys`.
///
/// The `session-maintenance` family needs a narrower list than the default
/// above: its `set_model` fixtures carry a `model` the CALLER chose, which is
/// request payload rather than a template-derived catalog fact, so `model`
/// there is load-bearing on purpose.
pub fn assert_no_fixture_observes(family: &str, keys: &[&str]) {
    for (name, fixture) in load_fixtures(family) {
        let mut offenders: Vec<String> = Vec::new();
        collect_keys(&fixture, &mut |key| {
            if keys.contains(&key) {
                offenders.push(key.to_string());
            }
        });
        assert!(
            offenders.is_empty(),
            "{name} observes {offenders:?}, which the northstar template only \
             placeholds. Derive them from the real model catalog before pinning them."
        );
    }
}

fn collect_keys(value: &Value, visit: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                visit(key);
                collect_keys(nested, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_keys(item, visit);
            }
        }
        _ => {}
    }
}

/// Every person id a fixture names must be one the template produces, or one of
/// the two ids the corpus uses to mean "a stranger".
pub fn assert_person_ids_come_from_the_template(family: &str) {
    let known: BTreeMap<&str, ()> = NORTHSTAR_PEOPLE.into_iter().map(|id| (id, ())).collect();
    for (name, fixture) in load_fixtures(family) {
        collect_person_ids(&fixture, &mut |person| {
            assert!(
                known.contains_key(person) || person == "nobody" || person == "human",
                "{name}: fixture names person '{person}', which the northstar template does \
                 not produce. Either the template changed or this runner's roster is stale."
            );
        });
    }
}

fn collect_person_ids(value: &Value, visit: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if matches!(
                    key.as_str(),
                    "personId" | "managerPersonId" | "assigneePersonId" | "requestedBy" | "to"
                ) {
                    if let Some(person) = nested.as_str() {
                        visit(person);
                    }
                }
                collect_person_ids(nested, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_person_ids(item, visit);
            }
        }
        _ => {}
    }
}
