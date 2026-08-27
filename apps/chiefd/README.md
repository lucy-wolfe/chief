# `chiefd` — Rust workspace

The nine crates that are the product. `chiefd` is the backend: **one daemon per
company directory**, owning that company's single SQLite database
(`<dir>/.chief/db/chief.db`), the supervisor loop, and every host-side effect.
`chief` is the client in the same workspace — it owns tmux and the terminal, and
it reaches a company only over HTTP.

A command finds its own company's daemon by reading the rendezvous file the
daemon writes into the directory, `<dir>/.chief/run/daemon.json`
(`host-primitives/src/rendezvous.rs`). That file is a POINTER, never authority: a
reader must still prove the pid is alive and the listener answers. There is no
registry on the path between a command and its own company. `beacond` is the
separate box-wide presence registry that answers "what is running anywhere on
this machine" for `chief ls` and the web app; nothing in the attach path consults
it.

**Status: shipping.** Every crate below is implemented and covered by the
workspace suite.

[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) is the current picture.
Where any document and the code disagree, **the code wins**.

## Crate boundaries

```
chiefd (bin)  ──▶ chiefd-api ──▶ chiefd-host ──▶ chiefd-core
                       └──────────────────────────────┘
```

| Crate | Owns | Explicitly does **not** own |
|---|---|---|
| `chiefd-core` | The deterministic half: writer actor and mutation scheduler, SQL schema, per-store ledgers and `validate()`, leases, failure polarity, the injected `Clock`. | Any host effect. No tmux, no pi, no filesystem. |
| `chiefd-host` | Everything chiefd does *to the machine*: the `HostExecutor` trait and its real tmux/pi implementation, host transactions (the DB↔filesystem 2PC), caller authentication mechanics (peercred, `/proc` ancestry, bearer tokens). | chiefd's own databases. It never opens one. |
| `chiefd-api` | The wire surface: request/response types with `deny_unknown_fields` and `schemars` derivation, `CallerIdentity`, the axum router and tiered readiness, the Phase-1 legacy SQL surface, the `chiefctl` client. | Business rules. Handlers translate; they do not decide. |
| `chiefd` | Wiring and process lifecycle only. | Policy of any kind. |
| `chief-cli` | The CLIENT, installed as `chief`: operator verbs, placement, actuation, tmux, and the terminal. It is the only program that speaks tmux. | Any of the four above. It depends on none of them and reaches a company only over HTTP. |
| `chiefd-log` | The daemon-level observability stream that sits above the per-company JSONL logs. | Company state. |
| `host-primitives` | The host answers BOTH actuators must give identically: the rendezvous file shape, redaction, process checks, shared error shapes. | Anything only one side needs. |
| `identity-keys` | A leaf: where a non-person identity key lives on disk and the 0600 mode it must have. | Reading or using a key. |
| `beacond` | The box-wide presence registry — which companies exist on this box and where each one's chiefd is. No auth, one SQLite table. | Any company's own state, and the attach path, which never consults it. |

Two boundaries carry most of the design's weight:

* **`HostExecutor` is the unit-test seam** (plan §4). Store-layer tests run
  against `FakeHostExecutor` with recorded calls and indexed failure injection,
  so ordering invariants are asserted by call order rather than by timing. Real
  tmux behaviour is tested only in `chiefd-host`'s own tests and the e2e
  harness.
* **The error taxonomy is split on purpose.** `chiefd_core::ChiefdError` is the
  semantic taxonomy the store layer produces; nothing in `chiefd-core` derives
  `Serialize`. `chiefd-api` writes the explicit wire projection at M2, so the
  two cannot drift by accident.

## The `test-support` policy

Test-only helpers live behind a **cargo feature named `test-support`**, declared
by every crate in the workspace and propagated downward
(`chiefd-api/test-support` → `chiefd-host/test-support` →
`chiefd-core/test-support`).

**Why a feature and not `#[cfg(test)]`.** The conformance runner, the
integration tests and the e2e harness are separate crates, and `cfg(test)` items
are invisible to them (plan §5.2 item 3).

**Why gated at all.** These helpers turn every documented wait into no wait:

* `chiefd_core::lease::RetryLadder::zero_wait()` — a ladder with no rungs.
* `chiefd_core::test_support::ManualClock` — time that only moves when a test
  moves it.
* (M9) the named pause points used for crash injection.

In a live company that is not a faster chiefd; it is the
fail-fast-with-no-retry shape this project has shipped three separate times.

**The rules.**

1. Anything that shortens, skips or fakes a wait, a clock, or a host effect goes
   behind `test-support`. Nothing else does.
2. Items are gated `#[cfg(any(test, feature = "test-support"))]`, so a crate's
   own unit tests see them without the crate depending on itself.
3. No crate may put `test-support` in its `[features] default`, and no
   `[dependencies]` entry may enable it — only `[dev-dependencies]` and the
   harness crates.
4. **CI asserts it.** The `chiefd` job resolves the dependency graph for a
   default (release) build and fails if `test-support` appears anywhere:

   ```sh
   cargo tree --workspace -e normal -f '{p} [{f}]' | grep -q 'test-support' && exit 1
   ```

## The seam lints

`clippy.toml` plus `[workspace.lints]` in `Cargo.toml` enforce plan §5.2 item 4:

* `unwrap_used`, `expect_used` and `panic` are **denied** in non-test code.
  `allow-unwrap-in-tests` / `allow-expect-in-tests` / `allow-panic-in-tests`
  keep test code readable without weakening production builds.
* `rusqlite::Connection::open` is **disallowed**. Only `chiefd_core::store` may
  open a connection, because the whole design rests on one writer thread per
  `chief.db`.
* `std::fs::write` / `remove_file` / `rename`, `std::fs::File` and
  `std::fs::OpenOptions` are **disallowed**. Filesystem effects belong to
  `chiefd_host`, inside a host transaction — a bare write outside one is a torn
  DB↔filesystem state waiting for a crash (plan §5.6).
* `std::thread::sleep` and `tokio::time::sleep` are **disallowed**. All waiting
  flows through the injected `Clock` so no test ever sleeps to wait for a
  timeout.

`clippy.toml` is workspace-global, so the legitimate owners carry a narrow,
commented `#[allow(clippy::disallowed_methods)]` at the exact call site. That is
deliberate: every place chiefd can open a connection or touch a file is one
`grep` away.

### Proving the lints still fire

`tests/seam-fixture/` is a crate whose only content is planted violations — a
direct `std::fs::write`, a `std::fs::OpenOptions`, an `unwrap`, an `expect`. It
carries its own `[workspace]` table so `cargo build --workspace` never sees it,
and CI runs clippy against it asserting a **non-zero** exit. If that crate ever
passes clippy, the seam has stopped protecting the real crates.

## Developing

The toolchain lives in `~/.cargo/bin`:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd apps/chiefd

cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test  --workspace --all-features
```

`--all-features` is what exercises the `test-support` helpers; the default
feature set is what ships.
