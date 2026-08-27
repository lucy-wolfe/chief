# `chiefd-request-schemas.json`

The canonical, drift-guarded copy of the schemars request-schema export
(E1-S3/#760). It was originally seeded byte-identical from
`shim/generated/chiefd-request-schemas.json`, but that was a one-time
transcription precondition, not a standing invariant — **do not `cmp` the two
files or try to restore parity between them.** See "Regeneration" below.

## What this covers — and what it does NOT

This file is the frozen, schemars-derived request-schema table for the
**`wire/*` op table** — the UDS transport heritage documented in
`shim/transport.ts`/`shim/client.ts`. That surface is **not served today**
(no live UDS listener exists); it is retained as a frozen snapshot.

It does **NOT** cover the live `/v1/*` docstore HTTP surface this package's
resource clients (`resources/*.ts`) talk to. There is no OpenAPI spec or
generated schema for that surface — its shapes are the hand-maintained,
Rust-authority-commented TypeScript interfaces under `src/types/*`, guarded by
this package's own contract tests (E2-S7), not by this file.

Do not use this file to validate a `/v1/*` request or response. Its `ops` keys
name `wire/*` operations, not `/v1/*` routes.

## Regeneration

As of E1-S3 (#760), the Rust test's `generated_path()` writes directly to
this file — regenerate with:

```sh
UPDATE_SHIM_SCHEMAS=1 cargo test --manifest-path apps/chiefd/Cargo.toml -p chiefd-api --test shim_schema_export
```

then confirm `git diff --exit-code packages/chiefing/generated/` is clean.
That is the invariant that matters: this file matches the Rust schemars
derivation. It is **not** checked against `shim/generated/chiefd-request-schemas.json`,
and it never will be again.

**`shim/generated/chiefd-request-schemas.json` is frozen legacy — never `cp`
it over this file, in either direction.** It is read only by the parked
legacy tree (`shim/schemas.ts`, `tests/shim.test.ts` and siblings) for as
long as the shim is the live extension client, and is deleted wholesale with
the shim by a later epic. Nothing in `@chief/chiefing` keeps the two files
equal at runtime or in CI (Mandate 0) — a parity assertion, or a parity
*habit*, is how a frozen copy quietly becomes a second maintained one.
