# Security policy

## Reporting a vulnerability

**Do not open a public issue.**

Report privately through GitHub's private vulnerability reporting: go to the
**Security** tab of this repository and choose **Report a vulnerability**. That
opens a private advisory visible only to you and the maintainers.


Please include:

- what an attacker gains,
- the smallest reproduction you have,
- the commit or release you tested,
- your platform and `tmux -V`.

**Expected response window: 5 business days.** We will confirm
receipt, tell you whether we consider it a vulnerability, and keep you updated
until it is fixed or closed.

We will credit you in the advisory unless you ask us not to.

## Supported versions

chief installs from source (`bun run release`). Security fixes land on `main`;
there are no maintained release branches.

## What counts as a security bug here

chief runs coding agents with the operator's real provider credentials, inside
the operator's own terminal, against the operator's own filesystem. That shape
decides the scope.

**In scope — these are the paths worth attacking:**

- **Credential leakage into any durable or visible surface.** A provider key
  lives in one place: a `0600` `.provider-credentials.json` inside the person's
  Pi home, read inside the Pi process. It must never travel as an argv value, an
  environment value, or a pane environment stamp, and it must never reach an
  error string, a health incident, the event journal, or a log line.
  `apps/chiefd/crates/host-primitives/src/redact.rs` is the last line of that
  defence — it masks conservatively on purpose, because a false positive costs a
  reader some context while a false negative writes an API key into a durable
  store. A way past it is a security bug.
- **The identity and bearer path.** Caller authentication lives in
  `chiefd-host` (peercred, `/proc` ancestry, bearer tokens). Anything that lets
  one caller act as another — a pane presenting a bearer it was not issued, a
  person acting outside its own subtree, a non-CEO acting on the CEO — is in
  scope.
- **The authority model.** Authority over structure is the subtree you head.
  Nothing may reach sideways at a peer or upward at a manager. A path that
  crosses either boundary is a security bug, not a product bug.
- **Cross-company isolation.** Each company is a directory with its own database
  and its own daemon. A caller in one company reaching another company's state,
  Pi home, or daemon is in scope.
- **Identity key handling.** `identity-keys` pins where a non-person key lives
  and the `0600` mode it must have. A path that writes one with wider
  permissions, or into a shared location, is in scope.
- **Host effects escaping their transaction.** Filesystem effects belong inside
  a host transaction (`chiefd-host/src/host_txn.rs`). A bare write outside one
  is a torn DB↔filesystem state waiting for a crash. Clippy denies the raw
  `std::fs` calls precisely so that every legitimate exception is one grep away.
- **`beacond`** listens on `127.0.0.1:6969` with **no authentication**, by
  design — it answers only "which companies exist and where is each one's
  chiefd". If it can be made to disclose more than that, or to be reached from
  off-box, that is in scope.

**Out of scope:**

- Anything requiring an attacker who already has your shell. chief's trust
  boundary starts at the operator's own account; a local root can do anything
  chief can.
- An agent doing something you did not want. chief runs agents you configured,
  with the tools you granted them. Prompt-injection-driven misbehaviour by an
  agent within its own granted capabilities is a product concern, not a
  vulnerability — **unless** it crosses one of the boundaries above (another
  person's subtree, another company, a credential, a bearer).
- Vulnerabilities in Pi itself or in `@earendil-works/pi-*` — report those
  upstream. We will help route them.
- Missing hardening with no demonstrated impact (headers, TLS on a loopback
  listener, dependency versions with no reachable path).

## What we do with a report

1. Confirm receipt.
2. Reproduce and decide severity.
3. Fix on `main`, with a regression test that pins the rule — the same
   requirement every other change here carries.
4. Publish a GitHub Security Advisory once a fix is available.
