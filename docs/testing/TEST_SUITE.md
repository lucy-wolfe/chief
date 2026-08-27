# The chief test suite

**Audience: an agent told only "run the chief test suite".** This document is
the whole instruction set. Read it top to bottom, then execute it top to bottom.
Every number, path, and command below is here because guessing it once cost
somebody real time.

This is a **live, end-to-end, real-user** suite. It does not run `cargo test`
and it does not read source to decide whether a feature works. It boots a real
machine on zipbox.ai production, installs a real build of `chief` on it, creates
a real company with Founder, and then **clicks and types in a browser** while
watching the actuator, the daemon log, the Pi panes, and tmux's own tags. The
repo's unit and guard suites answer "does this code work when called"; this
suite answers the independent question "does the product do the thing when a
person uses it" — the two claims are separate, and conflating them is the
failure mode `docs/verification-rules.md` Rule 1 exists to stop.

**Budget.** A clean pass is roughly 90 minutes: ~15 to boot and sign in, ~25 to
build and copy, ~15 for Founder, ~35 for the cases. A failure stops the run, so
a bad run is shorter, not longer.

---

## 0. Rules of engagement

These apply to every stage and every case. They are not advice.

| # | Rule | Why it exists |
|---|---|---|
| 0.1 | **Stop at the first unexpected mutation or visual failure.** Capture evidence, write the report, and stop. Do not "try once more", do not push on to the next case. | A second gesture on a broken rail overwrites the evidence of the first. Almost every hard bug in this repo's history was diagnosed from the state *at the moment it broke*, not from the state three clicks later. |
| 0.2 | **The suite is READ-ONLY with respect to any company it did not create.** Never stop, reset, wake, park, or `rm` a company you did not create in this run. Never kill a tmux session you did not create. | These boxes host other people's work. A company is a directory with a live daemon; killing the wrong session is indistinguishable from a crash to everybody watching it. |
| 0.3 | **Never treat an old machine's IP, socket, PID, database, or browser tab as belonging to a new one.** Re-derive every one of them after every boot. | Session names are `org-<slug>-<key6>_` where the key is a hash of the *directory*; a stale tab or a stale socket name resolves to something that exists and answers, which is worse than an error. |
| 0.4 | **Do not build on the target.** Build on the dev box, copy binaries. | The target is 2 vCPU / 4 GB. A cargo build there will swap the box to death and you will report a timeout as a product failure. |
| 0.5 | **Verify the artifact, not the intent.** After every copy, compare `sha256sum` on both sides. After every gesture, read tmux and the logs — never conclude from the screenshot alone. | A screenshot proves a pane painted. It does not prove which process painted it, which is exactly what several of the cases below are about. |
| 0.6 | **Re-measure before every click.** See §5.2. | The single most expensive rule in this document. |
| 0.7 | **Record UTC for everything.** `date -u +%Y-%m-%dT%H:%M:%SZ` before and after each gesture. | Log correlation across three logs and a browser is impossible without it, and the actuator prints a round line about once a second. |
| 0.9 | **EVERY gesture goes through the visible browser. ssh NEVER drives the UI.** Founder, the company, and every click and keystroke of every case happen in the zipbox sandbox tab in the box's own Chromium over CDP (`127.0.0.1:9222`). ssh is for building, copying, reading logs, tmux inventory and tags — nothing else. If you catch yourself running `chief` behind `ssh -t`, stop and move it into the browser. | The operator watches the browser: a run they cannot see is a run that did not happen for them, and it is the first thing they notice. It is also a different client attach path from the one users have, so a TUI driven over ssh has tested something users never do. This was violated on the suite's first real run — two agents ran the whole company inside their own ssh tmux session and the operator saw a blank tab. |
| 0.11 | **One runner per box, and the run OWNS `/root/.chief/bin`.** Before Stage B, confirm nobody else is on the target: no tmux server you did not create, no other company under `/root/companies`, no other agent's launcher root. If you must share, give your run its OWN bin directory and put it first on PATH. | `/root/.chief/bin` is a single shared path and every runner is told to rsync into it. On the suite's first run a second agent overwrote all three binaries mid-run at 19:30:31Z: processes started before that were the verified build, everything exec'd after was somebody else's, and five cases became mixed-binary and uncertifiable. A result you cannot attribute to a SHA is not a result. |
| 0.10 | **Exactly ONE attached chief client at a time.** Before starting chief in the browser, kill any tmux session you or a previous run left behind on the target. Never leave an ssh-side `chief` attached while the browser one runs. | Two attached clients put two rails in one window and squeeze the second to **1 column**. Every geometry number in this suite is then wrong, and the layout failure looks like a product bug — it has already been reported as one. |
| 0.8 | **`apps/web` is not part of this loop.** Do not start it, do not `bun run web:dev`, do not open `localhost:3000`. | chief is a tmux TUI. The browser in this suite is a *terminal*, not the web client. The web client is a separate host with its own suite. |

---

## 0.5 Stage ZERO — start from nothing

> **QA / TEST BOXES ONLY. NEVER run this section against a box carrying real
> work.** Every command below destroys state. The operator has ruled that live
> boxes are never repaired or mutated by a test run, and this is exactly the
> text somebody will one day paste into the wrong terminal. If you are not
> certain the target is a disposable QA box, you are not allowed to run Stage
> ZERO — stop and ask.

**A run that does not start from nothing cannot tell you what it proved.** This
is not tidiness. One run on 2026-08-18 inherited, all at once: a stale
`launcher-root` pointing at a worktree that no longer matched, stale binaries
from an earlier build, a dead tmux server holding an attached client, and
leftover company directories. **Every one of those produced a misleading signal
before anybody noticed it**, and one of them (the attached client) would have
tripped rule 0.10 and been reported as a product layout bug.

The operator's instruction: *"you need to always start from scratch like delete
everything clean up everything and then start from scratch … and then when
you're done you need to clean up after yourself."*

**Do not assume the box is clean because the last run said it finished.**

### 0.5.1 Tear down

```bash
ssh root@$TARGET '
  # 1. EVERY tmux server, on EVERY socket. Enumerate -- never hardcode.
  for d in /tmp/tmux-*/; do
    for sock in "$d"*; do
      [ -S "$sock" ] || continue
      tmux -S "$sock" kill-server 2>/dev/null && echo "killed server: $sock"
    done
  done

  # 2. EVERY chief/chiefd/beacond/pi process. There is NO pgrep on these
  #    boxes -- walk /proc/*/exe, which is the real binary and cannot be
  #    spoofed by a command name.
  #
  #    STRIP THE " (deleted)" SUFFIX FIRST. See the note below 0.5.2: a
  #    process whose binary was replaced under it reads
  #    `/root/.chief/bin/beacond (deleted)`, and without this line the
  #    basename is `beacond (deleted)`, which matches nothing here.
  for p in /proc/[0-9]*; do
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    exe=${exe% (deleted)}
    case "$(basename "$exe")" in
      chief|chiefd|beacond|pi|node)
        cmd=$(tr "\0" " " < "$p/cmdline" 2>/dev/null)
        case "$exe$cmd" in
          *chief*|*beacond*|*/bin/pi*|*node_modules/.bin/pi*)
            echo "killing ${p#/proc/}: $exe $cmd"; kill -9 "${p#/proc/}" 2>/dev/null ;;
        esac ;;
    esac
  done

  # 3. Company directories -- LIST FIRST (see the warning below), then remove.
  ls -la /root/companies/ 2>/dev/null

  # 4. Binaries and launcher root, so a stale build can never be mistaken
  #    for the one under test.
  rm -rf /root/.chief/bin /root/.chief/launcher-root
'
```

**NEVER `rm -rf` a companies root without listing it first.** A box may hold a
company that is not yours. On 2026-08-18 a live box held `suiterun-labs` from an
earlier run **and** `acceptance-labs` whose directory was already gone — and
`chief ls` reported the dead one as `unknown` **while printing a live
neighbour's daemon URL**. An operator or an agent trusting that output would act
on the wrong company. Read the listing, name every directory you are about to
delete in the run record, then delete them by name.

### 0.5.2 VERIFY the teardown — do not trust it

A teardown nobody checks is how the 2026-08-18 run inherited a dead `default`
server with an attached client. Re-enumerate and assert **zero** of each:

```bash
ssh root@$TARGET '
  echo "servers:  $(ls -d /tmp/tmux-*/ 2>/dev/null | wc -l)"
  echo "sockets:  $(find /tmp/tmux-*/ -type s 2>/dev/null | wc -l)"
  echo "procs:    $(for p in /proc/[0-9]*; do readlink "$p/exe" 2>/dev/null; done \
                    | sed "s/ (deleted)$//" \
                    | grep -cE "/(chief|chiefd|beacond|pi)$")"
  echo "companies: $(ls /root/companies/ 2>/dev/null | wc -l)"
  echo "binaries:  $(ls /root/.chief/bin/ 2>/dev/null | wc -l)"
'
```

**`sed "s/ (deleted)$//"` IS THE CHECK, NOT TIDINESS — DO NOT SIMPLIFY IT AWAY.**
Both readings above walk `/proc/<pid>/exe`, and the kernel appends a literal
` (deleted)` to that link when the binary the process is running has been
UNLINKED since it started. Stage B does exactly that: every run rsyncs new
binaries over `/root/.chief/bin`, so **a daemon left behind by the PREVIOUS run
has a deleted exe from the moment you copy**. Without the strip, its link reads
`/root/.chief/bin/beacond (deleted)`, the `$`-anchored pattern here does not
match it, `basename` in §0.5.1 yields `beacond (deleted)` and matches no arm
there either — so the leftover is neither killed nor counted, and the teardown
**reports a clean box while a daemon from the previous run is still on it.**

Measured on a live box, 2026-08-19: `beacond` pid 20305, started 01:44 by the
previous run, survived a full Stage ZERO that printed `procs: 0`, and was still
alive four hours later. Every "Stage ZERO verified" reading taken that night was
taken with this blind spot. It is the worst shape a check can have — the one
that answers `0` for the wrong reason — and §0.5's whole argument is that an
unverified teardown becomes the next run's inherited state. Strip the suffix, or
match on the path prefix instead of the exact basename; do not delete the line
because the pipeline "looks like it does nothing".

**All five must read 0.** Any non-zero reading stops the run: investigate what is
holding the box before Stage A, and record it. Note that the socket names are
NOT predictable — today's runs found `acc`, `default`, `founder` and `suite`
alongside the per-company hex key production is supposed to use, so a hardcoded
socket list will miss one and the one it misses is the one that bites.

Record all five readings in the run report. A run that cannot show a clean Stage
ZERO is reporting against unknown inherited state and its verdicts are not
verdicts.

---

## 1. Stage A — boot a fresh machine on zipbox.ai (production, through the browser)

### 1.1 Preconditions

Read your harness's own browser and desktop skill documents — on this
project's development boxes they are `$SKILLS/zipbox-browser/SKILL.md` and
`$SKILLS/zipbox-desktop/SKILL.md` — **before** touching a browser.
**Operators: substitute your own paths.** This section is written against one
particular sandbox; the STEPS are the contract, the paths are not.
This stage is done in the **box's own visible Chromium**, over CDP, so the user
can watch it happen:

```bash
zipbox-desktop status || zipbox-desktop up
playwright-cli -s=desktop attach --cdp=http://127.0.0.1:9222
```

**Never launch a fresh headless browser for this suite.** The point of the run
is that a human watches a machine do the thing. A headless run proves the same
code paths and demonstrates nothing. If `zipbox-desktop up` refuses (a box under
the 3 GiB floor prints exactly that), the suite cannot be run from this box —
report the named reason and stop; do not silently fall back to headless.

Do not `close` the attached session when you finish. Use `detach` — `close`
takes down the user's browser with everything they had open in it.

### 1.2 Sign in

1. `playwright-cli -s=desktop goto https://zipbox.ai/dashboard`
2. Submit the sandbox's own email address.
3. Privy answers with a 6-digit OTP in that mailbox. Read it with the baked CLI:

   ```bash
   tribes-email list
   tribes-email read <uid> --uid-validity <uidValidity>
   ```

   `read` takes the uid **positionally**, and `--uid-validity` is **required**.
   The output is JSON with escaped `\n` newlines — pipe it through
   `python3 -c 'import json,sys;print(json.load(sys.stdin)["text"])'` rather than
   reading the raw escapes.
4. Enter the code. Screenshot the signed-in dashboard.

### 1.3 Boot the sandbox

"Boot your first sandbox" / "New sandbox", then:

- **Reuse an existing identity from the "Pick an identity on hold" dropdown.
  Never type a new name.** Typing a name mints a new identity; the on-hold pool
  exists so a test run does not leak one per run.
- Pick an agent.
- Open **Advanced options** and paste this box's public key into the SSH field:

  ```bash
  cat /root/.ssh/id_ed25519.pub
  ```

  Without this you cannot ssh in afterwards, and every later stage of this suite
  is over ssh. There is no way to add the key after Boot.
- Boot.

### 1.4 Record the machine, before anything else touches it

```bash
TARGET=<your-build-host>
ssh -o BatchMode=yes root@$TARGET 'hostname; date -u +%Y-%m-%dT%H:%M:%SZ; nproc; grep MemTotal /proc/meminfo; ldd --version | head -1'
```

Write all five into the run record. The ssh command working **is** the gate for
Stage B — if it does not answer, the SSH key did not land and you must reboot a
machine with it rather than working around it.

`ldd --version` is not decoration: §2.3 refuses to copy binaries across a glibc
mismatch, and this is the reading it compares against.

**Then check the two things the target needs and this suite does not install.**
Both were discovered by the suite's first real run, and both fail in a way that
looks exactly like a product bug:

```bash
ssh root@$TARGET 'node --version 2>/dev/null || echo NO-NODE; ls /run/zipbox/placeholders.env'
```

| Requirement | What it looks like when missing |
| --- | --- |
| **A JavaScript runtime, Node ≥ 20.** `node_modules/.bin/pi` is `#!/usr/bin/env node`, and chief probes `pi --version` before it will launch anybody. | `chief` refuses outright: *"Pi is required but no runtime was found."* A fresh zipbox box may have NO node and NO bun at all. Node 18 is worse than none — it installs cleanly and then dies inside Pi with an ESM module-loader error, so check the VERSION, not just the binary. |
| **`/run/zipbox/placeholders.env`, sourced.** It carries this box's own provider keys. | Founder opens with *"No models available"* and the run dead-ends in Stage C with nothing that looks like a cause. |

Source the placeholders in every shell that will start chief or a person:
`set -a; . /run/zipbox/placeholders.env; set +a`. Use the TARGET's own file —
never carry a key between boxes.

Also confirm the operator Pi agent directory has a usable provider
(`/root/.pi/agent/models.json`). chief's launch gate now refuses a person whose
agent home reaches neither `auth.json` nor `models.json` (`b582b20b5`, Case 23),
but it holds no credential and cannot tell you whether the one it finds works.

**Then establish CA trust, and read `000` correctly.** On a zipbox box outbound
HTTPS is intercepted and re-terminated by the platform, which presents a
certificate signed by a CA that is **not in the system trust store**. The CA is
on disk at `/run/zipbox/ca-bundle.crt`, and **nothing exports it**:

```bash
ssh root@$TARGET 'set -a; . /run/zipbox/placeholders.env; set +a
  ls -l /run/zipbox/ca-bundle.crt
  echo "NODE_EXTRA_CA_CERTS=[${NODE_EXTRA_CA_CERTS:-UNSET}] SSL_CERT_FILE=[${SSL_CERT_FILE:-UNSET}]"'
```

**Both variables read `UNSET` in a shell that has sourced `placeholders.env`.**
That is the expected reading, not a fault — record it, because it is the fact
that makes the next table necessary. chief *forwards* `NODE_EXTRA_CA_CERTS`,
`SSL_CERT_FILE`, `CURL_CA_BUNDLE` and `REQUESTS_CA_BUNDLE` into each pane when
the host shell already exported them, and on a zipbox box the host shell never
does (`issues/ca-bundle-trust-is-inherited-not-supplied.md`). So supply the
bundle explicitly in every credential check this suite makes:
`--cacert /run/zipbox/ca-bundle.crt` for curl, `NODE_EXTRA_CA_CERTS` for node.

**THE READING TABLE — commit it to memory before §2.5.** A TLS trust failure and
an out-of-credit failure look identical from a pane, and the difference is the
CLOCK:

| Reading | Meaning | What it is NOT |
| --- | --- | --- |
| `http=000` in **~20 ms** | **CA trust.** The handshake was rejected immediately — the bundle is not in that shell. Re-run with `--cacert /run/zipbox/ca-bundle.crt`. | Not credit. Not the network. Not a rate limit. Twenty milliseconds is too fast to be any of them. |
| `http=000` after **seconds** | A real network or DNS failure, or a timeout. | Not the CA bundle. |
| `http=402` | **Credit.** The wallet is out. Stop and get it funded. | |
| `http=401` | Wrong or absent key. | |
| `http=200` | Good. Proceed. | |

**This misreading has already cost real money.** A bare `curl` returning `000` on
a healthy, fully funded box was read as a credit failure, and the user added
credit to an account that was never short of it — the wallet answered `200`
throughout. A bare `curl` is not a credential test on this platform, and any
reading taken without `--cacert` is uninterpretable.

**This section MEASURES the problem; §3.3 is where you FIX it.** Supplying the
bundle to your own `curl` proves the box is healthy and does nothing for the
company — chief forwards the CA variables into a pane only when the host shell
already exported them, so the export has to be in the shell that starts `chief`.
Do not stop at a green reading here: a §2.5 that passes and a §3.3 that was
skipped is precisely the combination where every person dies at the TLS
handshake while every surface chief owns reports a healthy company.

---

## 2. Stage B — get chief onto the target

### 2.1 Build on the dev box, with an isolated HOME

Work on the dev/build box — **whichever box you were told to use** (some
boxes may be off-limits; follow your assignment) — in a
**clean worktree at the SHA under test**:

```bash
cd /root/workspace/chief && git fetch origin main
git worktree add /root/worktrees/<run-id> <SHA-under-test>
```

Then build with a HOME that is not the build box's own:

```bash
mkdir -p /root/relhome
ln -sfn /root/.cargo /root/relhome/.cargo
ln -sfn /root/.rustup /root/relhome/.rustup
ln -sfn /root/.bun /root/relhome/.bun
ln -sfn /root/.cache /root/relhome/.cache
cd /root/worktrees/<run-id> && HOME=/root/relhome nice -n 10 bun run release
```

**Why the isolated HOME.** `bun run release` publishes into `$HOME/.chief` and
writes `$HOME/.chief/launcher-root`. Run with the build box's real HOME it
overwrites that box's own working install — including whatever the person next
to you is running. The four symlinks keep the caches shared so the build is
still incremental.

Use `bun run release`, never `release:fast`. The fast profile is dev-tuned and
the README says in as many words: never ship a binary built that way. A suite
that certifies a fast build has certified nothing shippable.

### 2.2 What the build produced

```bash
cat /root/relhome/.chief/launcher-root      # the recorded launcher root
ls -l /root/relhome/.chief/bin              # chief, chiefd, beacond
sha256sum /root/relhome/.chief/bin/*
```

`launcher-root` is the path the build **recorded**, and it is load-bearing on
the target: chief resolves Pi at `<launcher-root>/node_modules/.bin/pi`. If that
absolute path does not exist on the target, Pi cannot be resolved and every
person pane fails to start — with a failure that looks like a product bug and is
not one.

### 2.3 Copy

Compare glibc first:

```bash
ldd --version | head -1                                   # build box
ssh root@$TARGET 'ldd --version | head -1'                # target
```

**They must match.** A Rust binary built against a newer glibc dies on the older
box with a symbol-version error at exec time, which surfaces as a pane that
opens and instantly exits — again, a product-looking failure that is not one. If
they differ, stop and rebuild on a matching image.

Back up anything you are about to replace:

```bash
ssh root@$TARGET 'test -d /root/.chief/bin && cp -a /root/.chief/bin /root/.chief/bin.bak-$(date -u +%Y%m%dT%H%M%SZ) || true'
```

Copy the three binaries and the repo tree. **`rsync` on these boxes needs an
explicit remote path**:

```bash
RSP=--rsync-path=/opt/zipbox/runtime/usr/bin/rsync
LR=$(cat /root/relhome/.chief/launcher-root)

ssh root@$TARGET "mkdir -p /root/.chief/bin $LR"
rsync -a $RSP /root/relhome/.chief/bin/ root@$TARGET:/root/.chief/bin/
rsync -a $RSP --exclude apps/chiefd/target --exclude .git \
  /root/worktrees/<run-id>/ root@$TARGET:$LR/
ssh root@$TARGET "printf '%s\n' '$LR' > /root/.chief/launcher-root"
```

The repo tree must land at **the same absolute path the build recorded**, not a
convenient one. Copying it to `/root/chief` while `launcher-root` says
`/root/worktrees/<run-id>` produces a target where Pi is unresolvable.

### 2.4 Verify the copy

```bash
sha256sum /root/relhome/.chief/bin/{chief,chiefd,beacond}
ssh root@$TARGET 'sha256sum /root/.chief/bin/{chief,chiefd,beacond}; test -x '"$LR"'/node_modules/.bin/pi && echo pi-ok'
ssh root@$TARGET 'export PATH=/root/.chief/bin:$PATH; chief --help >/dev/null && echo chief-ok'
```

All three digests must match exactly, `pi-ok` must print, and `chief --help`
must exit 0. Any mismatch is a Stage B failure — report it as an installation
failure, not a product failure, and do not proceed.

---

### 2.5 Credential smoke test — one person must be able to think

**Do this before Stage C.** It costs seconds and it is the difference between
finding a dead provider now and finding it thirty minutes later at Case 6.

**`--cacert` is not optional and it is not caution.** Without it this test
returns `000` on a perfectly healthy funded box, and `000` has already been read
as a credit failure once — see §1.4's reading table, which this section is the
first user of. Run BOTH commands; two independent clients is what tells a
credential problem from a trust problem.

```bash
ssh root@$TARGET 'set -a; . /run/zipbox/placeholders.env; set +a
  curl -s -o /dev/null -w "http=%{http_code} time=%{time_total}\n" \
    --cacert /run/zipbox/ca-bundle.crt \
    -X POST https://openrouter.ai/api/v1/chat/completions \
    -H "Authorization: Bearer $OPENROUTER_API_KEY" -H "content-type: application/json" \
    -d "{\"model\":\"anthropic/claude-sonnet-4\",\"messages\":[{\"role\":\"user\",\"content\":\"ok\"}]}"'
```

And the same request through the runtime a person actually uses, because Pi is
node and node reads a different variable than curl does:

```bash
ssh root@$TARGET 'set -a; . /run/zipbox/placeholders.env; set +a
  NODE_EXTRA_CA_CERTS=/run/zipbox/ca-bundle.crt node -e "
    fetch(\"https://openrouter.ai/api/v1/chat/completions\", {
      method: \"POST\",
      headers: { authorization: \"Bearer \" + process.env.OPENROUTER_API_KEY,
                 \"content-type\": \"application/json\" },
      body: JSON.stringify({ model: \"anthropic/claude-sonnet-4\",
                             messages: [{ role: \"user\", content: \"ok\" }] })
    }).then(r => console.log(\"http=\" + r.status))
      .catch(e => console.log(\"ERR \" + e.message))"'
```

| Reading | Meaning |
| --- | --- |
| `http=200` | Good. Proceed. |
| `http=000` in ~20ms **with `--cacert`** | Worse than the untrusted case: the bundle is on disk and still did not satisfy the handshake. Stop and report it — do not work around it. |
| `http=000` in ~20ms **without `--cacert`** | Expected on this platform, and **not a finding**. It says only that the shell has no CA bundle. Re-run WITH `--cacert` before concluding anything. |
| `http=402` | **The wallet is out of credit. STOP.** Nothing that needs an agent to think can be tested, and the failure is invisible until you are deep in the cases. Report it and get the wallet funded before spending an hour. |
| `http=401` | Wrong or absent key. Check `/run/zipbox/placeholders.env` on the TARGET. |
| node `ERR ... certificate` while curl says `200` | Trust is per-client. The panes are node, so this is a company-wide failure even though curl is happy. |

**Never report a credit failure off a bare `curl`.** A `402` is the only reading
that means credit. `000` means trust until proven otherwise, and proving it costs
one flag.

**A green reading here does not mean the company can think.** It means THIS
SHELL can. The pane inherits its trust from the shell that started `chief`, not
from this one — see §3.3, and confirm it reached a real pane before you believe
any case.

A 402 does not merely fail turns: a woken person with no work has their launch
intent lapse in about five seconds (`nothing-demanded-them`) and their pane is
reaped, so you cannot hold two people live in one department and every case
about a live team becomes untestable. The first run lost a whole pass to
discovering that at Case 4.

Then check the three digests again — see 0.11. A concurrent agent rsyncing into
`/root/.chief/bin` invalidated part of the first run, and the only way to know is
to re-read them:

```bash
ssh root@$TARGET 'sha256sum /root/.chief/bin/{chief,chiefd,beacond}'
```

**Re-read them before each case block, not only once.** A digest that changed
mid-run means every case since the last check is mixed-binary and must be
reported as such rather than as a verdict.

---

## 3. Stage C — create a company with Founder

### 3.1 Founder is the only door

Bare `chief` in a directory **without** `.chief/db/chief.db` opens Founder. That
is the only local company-creation door there is: `chief create` and `chief new`
are deleted, not deprecated. There is no flag, no JSON, and no API shortcut that
skips it.

### 3.2 Name the directory the same as the slug

**The company directory's basename must equal the company slug.** The deploy
helper refuses otherwise, in those words:

```
COMPANY_DIR must end with the selected slug '<slug>'
```

(a company-scoped deploy check, since removed). A directory `acceptance` holding a
company slugged `acceptance-labs` will pass Founder and then refuse every
company-scoped deploy check afterwards. Decide the slug first, then name the
directory after it.

### 3.3 Run Founder

`chief` refuses to start outside tmux, and it exits if the tmux session it was
started from dies. So: an **interactive** terminal, **inside tmux**.

**That terminal is the zipbox sandbox tab in the visible Chromium — not ssh.**
Creating a company IS a user action and the operator watches it happen (rule
0.9).

**Verify before you start, because the tab is not tmux-backed on every box:**

```bash
echo "${TMUX:-EMPTY}"
```

If it prints `EMPTY` the tab is NOT tmux-backed on this box, and bare `chief`
will exit with `could not attach this terminal to ChiefD session
'chiefd-founder' (tmux exited 1)`. Run `tmux new -s host` in the tab first, then
carry on. **Do not read that exit as a product failure** — it is chief refusing
to start outside tmux, by design, and it has now cost two runs real time. One
box measured `$TMUX` empty here; that does not establish the tab is never
tmux-backed, which is why the instruction is the CHECK and not a claim either
way.

In the sandbox tab: `/exit` out of the agent to a shell (or click the
bottom-left corner for Bash), then type, one line at a time:

```bash
export PATH=/root/.chief/bin:$PATH
set -a; . /run/zipbox/placeholders.env; set +a
export NODE_EXTRA_CA_CERTS=/run/zipbox/ca-bundle.crt
mkdir -p /root/companies/<slug> && cd /root/companies/<slug>
chief
```

**The middle two lines are not diagnostics — they are the run.** §1.4 measures
the CA problem; this is where it is FIXED, and the fix has to be in the shell
that starts `chief`, because that is the environment every person inherits.
chief forwards `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE`, `CURL_CA_BUNDLE` and
`REQUESTS_CA_BUNDLE` into a pane **only when the host shell already exported
them**, and no zipbox shell exports any of them. Start `chief` from a shell that
did not, and every reading in §2.5 can still be green while every person in the
company dies at the TLS handshake — Pi prints its own error inside its own pane
and nowhere else, which is indistinguishable from a dead provider on every
surface chief owns.

Measured, on a box whose provider answered `200` all along: with the export,
Founder opens carrying `openrouter • anthropic/claude-sonnet-4` in its status
line and a company can be created on the first instruction. Without it, the same
box gives a Founder that cannot complete a turn.

So confirm it reached the pane, once, before you trust any case:

```bash
ssh root@$TARGET 'export PATH=/opt/zipbox/runtime/usr/bin:$PATH
  tmux -L <socket> capture-pane -p -t <a person pane> | tail -3'   # no TLS error
ssh root@$TARGET 'tr "\0" "\n" < /proc/<a pi pid>/environ | grep CA_CERTS'
```

A person pane whose environment carries no `NODE_EXTRA_CA_CERTS` on this
platform is a person who cannot think, whatever the rail says about them.

Use `pwcli -s=desktop` to click the terminal input and type each line, exactly
as you will for every gesture in §6.

If you have already left a `chief` or a tmux session on the target from an
earlier attempt, kill it FIRST (rule 0.10) — two attached clients corrupt every
geometry measurement in this suite:

```bash
ssh root@$TARGET 'export PATH=/root/.chief/bin:/opt/zipbox/runtime/usr/bin:$PATH; \
  cd /root/companies/<slug> 2>/dev/null && chief stop; tmux kill-server 2>/dev/null; true'
```

**Founder creates the company. It does NOT staff it.** Its only tool is
`chiefd_launch_company(name, purpose)` — it has no tool for departments or
people and will tell you so. Staffing is the CEO's job, after boot. The suite
said otherwise on its first run and the runner lost time discovering it.

So it is two instructions, to two different agents.

**To Founder**, one message: the company name (matching the directory) and its
purpose. Wait for it to confirm, then record the slug and the directory.

**To the CEO**, once the company is up and you are attached: one message naming

- one department,
- that department's manager,
- **at least four more people in it** — four or five live members is what makes
  case 4 a grid rather than a row, and two of anything cannot tell the two
  apart,
- **and that everyone is to be left asleep.**

Leaving everyone asleep is what makes the later cases meaningful: case 5 needs a
sleeping person to select, case 4 needs a department whose people are not up,
and case 6 needs exactly one wake to attribute a POST to. A company that boots
everybody wide awake cannot distinguish "the click woke them" from "they were
already up".

Then record the roster (names, handles, titles, department) from the store —
see §4.1 for why you read it from the database and not from an HTTP route.

### 3.4 Confirm it is on the glass, in the browser the user is watching

Founder ran in the sandbox tab, so the company is already there. Confirm it
before going on, because everything after this depends on it:

1. The rail is drawn in the browser tab — read it back out of the DOM, do not
   trust the screenshot alone (rule 0.5).
2. Exactly one chief client is attached (rule 0.10):

```bash
ssh root@$TARGET 'export PATH=/opt/zipbox/runtime/usr/bin:$PATH; tmux ls'
```
   You want the company session and its actuator session, and NO leftover
   session of your own (`host`, `founder`, `probe`, …). If one is there, you
   have two clients: stop, kill it, and re-read the geometry.

From here on, **every gesture in the suite is a click or a keystroke in that
browser terminal** (rule 0.9). ssh stays open in a second channel for reading
logs and tmux, never for driving the UI.

---

## 4. The watch list

Half of this suite is watching. This section is the reference every case points
back at. Open all four watchers before case 1 and keep them open.

### 4.1 Derive the names — never hardcode them

```bash
SLUG=<slug>; DIR=/root/companies/$SLUG
# Sessions (discovery aid, not authority):
tmux -L <socket> ls
```

**chiefd's HTTP routes are NOT available to this suite.** Every
`curl -XPOST $DAEMON/v1/org/*/read` answers `missing bearer token`, and the keys
under `.chief/keys/` are EC private keys for request SIGNING, not bearer tokens —
there is no `chief` subcommand that prints a roster either. The suite's first run
lost time on this. Read the store directly instead; it is the same data and it is
read-only:

```bash
DB="file:$DIR/.chief/db/chief.db?mode=ro"
python3 - <<PY
import sqlite3
db = sqlite3.connect("$DB", uri=True)
for row in db.execute("SELECT id, department_id, employment_state FROM people ORDER BY id"):
    print(row)
print("launch intent:", [r[0] for r in db.execute("SELECT person_id FROM launch_intent")])
PY
```

`sqlite3` is often absent on these boxes; the `python3` form above always works.
Anywhere below that says "read it from `/v1/org/<x>/read`", read the
corresponding table this way instead.

Session names are:

| Thing | Name |
|---|---|
| Company session | `org-<slug>-<key6>_` |
| Actuator session | `chiefd-actuator-org-<slug>-<key6>_` |

`<key6>` is the first six hex characters of the company **key**, which is a hash
of the company directory — not of the slug. Two directories may hold companies
with the same name; the key is what separates them, and the trailing `_` is a
terminator so tmux's prefix resolution cannot answer a probe for `acme` with a
running `acme-corp`. **Derive both names by listing sessions and matching the
prefix `org-<slug>-`. Do not compose them from the slug alone** — a name you
composed wrongly still resolves to *something*, and that something is another
company.

### 4.2 The actuator round line

```bash
tmux -L <socket> capture-pane -p -S -200 -t chiefd-actuator-org-$SLUG-<key6>_
```

It prints roughly once a second. Read it as follows:

| Line | Meaning | Verdict |
|---|---|---|
| `<company>: converged · N up` | Observed == desired. `N` counts **the DESIRED people who have a pane** — not every tagged pane, and not the count chiefd wants. | Healthy |
| `<company>: NOT converged · chiefd wants N people and tmux holds M; this plan asked for NOTHING, so the K missing will not be started by it` | The planner produced **no steps while people are missing**. | **FAILURE.** Capture the plan (§4.6) and stop. |
| `<company>: the pass FAILED after k of n step(s); nothing beyond that was attempted …` | A step errored mid-plan. | **FAILURE.** Capture the step and the pane it names. |
| `Quarantined stray tmux pane <id> in organization '<org>': not fully ownership-tagged; …` | A pane the actuator could not attribute. **Once** during a wake is a transient. **Every pass** is a fault. | Repeating → **FAILURE** |

Why this line is trusted — and why you still count the panes yourself. It was
wrong three separate ways inside 2026-08-18. It printed `converged · 17 up` once
a second for an hour while tmux held seven people, because it counted the people
chiefd WANTS. Then `converged · 11 up` while six panes carried a person, because
the empty-plan return claimed it had never looked. Then `converged · 13 up`
against thirteen tagged panes of which only eight belonged to somebody chiefd
still wanted — a departed or stale-but-tagged pane is `owned` and counts, so the
raw pane count and the desired count are two different sets and comparing them is
arithmetic across a gap.

The count therefore now answers exactly one question: **how many of the DESIRED
people have a pane.** Reproduce that intersection yourself (§4.5 and case 2)
rather than trusting the number — a stale tagged pane in your own count puts you
back on the bug this line has already had.

### 4.3 The daemon log

```bash
tail -f $DIR/.chief/run/daemon.log
```

**Count wakes before and after every single gesture.** Several cases below pass
or fail on that count alone. Count **two different facts**, because one of them
alone cannot answer the question:

```bash
# the REQUEST -- the literal subject of Case 6's claim, "exactly one signed request"
wake_posts()   { grep -c 'path=/v1/org/person/wake' "$DIR/.chief/run/daemon.log"; }
# the APPLIED wake -- what actually happened as a result
wake_applied() { grep -c 'event="org.person.wake.applied"' "$DIR/.chief/run/daemon.log"; }
BEFORE="$(wake_posts)/$(wake_applied)"; <gesture>; sleep 2
echo "$BEFORE -> $(wake_posts)/$(wake_applied)"
```

**Count `applied`, never `applied` plus `recalled`.** `org.person.wake.recalled`
is the SAME wake observed one layer down; adding them double-counts every wake
and turns every correct single wake into a reported double.

**Why both.** A refused wake is a request that never applies, so counting only
`applied` calls a double-click-with-one-refusal a pass; counting only requests
cannot tell a wake that landed from one that was thrown away. One click must
move BOTH by exactly one.

**THIS CHECK IS COUPLED TO A LOG LEVEL, and it has already been broken once by a
change nobody connected to it.** `d8f4e7714` demoted every fast 2xx to DEBUG,
which took `POST /v1/org/person/wake` with it, and for the hours that shipped
this grep returned **0 before and 0 after a wake that demonstrably happened** —
so the instrument for "exactly one signed request" read zero no matter what
occurred, and would have called a genuine DOUBLE wake a pass. `e59d43042`
restored it: the demotion is now an explicit allowlist of the paths the daemon
polls ITSELF with (`POLLING_READ_SEGMENTS` in `chiefd-api/src/docstore/router.rs`
— `read`, `read-person`, `desired`, `watch`, `launch-catalog`, `agent-state`,
`health`), and everything else, every mutation included, stays at INFO.
`docstore_request_log_level.rs` pins `level(200, 0, "/v1/org/person/wake") ==
INFO` so it cannot be demoted again silently.

**If both counts read 0 across a wake you watched happen, suspect the log level
before you suspect the product** — read the file at `--log-level debug` and see
whether the line is there. A check that returns a plausible `0` is worse than
one that errors, and this one has done it.

Also grep the window around each gesture for:

- `refus` — a refusal. Read the whole line; a refusal with a reason is often
  correct behaviour (see case 7), and a refusal repeating every round is not.
- `park` / `recall` — an unexpected park during a case invalidates that case's
  pane inventory.
- `reconcile actuation pass` — **the actuation record, and the line to read
  when people appear or disappear.** Its `notes=` field names the decision:
  `launching: <names>`, `mail wake granted launch intent: <names>`, `launch
  intent withdrawn (settled): <names>`, `mail demand NOT desired: <detail>`,
  `stand-down holds pending mail for: <names>`. A pass logs at INFO when it
  recorded something new **or** when it made any of those decisions; a steady
  company that merely re-affirms its desired set logs at DEBUG and is silent.

  The second half of that rule is new, and the incident that bought it is the
  clearest statement of why an absent record is a question rather than an
  answer: a company relaunched six people forty-five seconds after an operator
  stood it down, and `daemon.log` for that window held nothing but `supervision
  cycle committed`. The grant note existed on the report the whole time. It was
  written at DEBUG, because the level asked only whether the pass had recorded
  something NEW — and a wake grant for somebody the desired set already named
  records nothing new. **If you are asking "who launched, and why", this is the
  line.**

- reconcile lines carrying **`desired=`**. That is the whole of what the line
  carries now, and the number is how many people chiefd wants. Cross-check it
  against the store rather than reading it alone (Case 24):
  `sqlite3 -readonly $DIR/.chief/db/chief.db "select count(*) from people where
  slug='$SLUG' and employment_state='active';"`. **The round line is the
  authority** on whether a pass converged; this number is only what it was
  asked for.

  **`planned=` and `actuated=` no longer exist. Do not grep for them.** They
  were what this section told runners to read until `0daa36b0b` deleted both:
  `planned=` was `desired_people` under a name it had two designs ago, and
  `actuated=` was a field chiefd could only ever report as 0, because it emits
  no actions and applies none. On the first real run every line said
  `planned=N actuated=0` across passes where people demonstrably came up, and
  this document taught a heuristic for interpreting that phantom. A grep for
  either word now matches nothing, for ever, and an empty match is not a
  finding — it is this doc being out of date. Case 24 asserts their absence.

  **AN ABSENT RECONCILE RECORD IS ITSELF A FINDING, and it is the one this
  section is worst at surfacing.** Every reading in this document is a
  `grep` — and a grep answers "not found" identically for *this did not
  happen*, *this happened and was not recorded*, and *this doc is looking for
  the wrong string*. Those are three different worlds and only the first is a
  product verdict. So when a line you expected is missing, you have not
  finished: say which of the three you are claiming, and show the evidence
  that rules out the other two. Re-read at `--log-level debug`, and check the
  string against the source before concluding anything about the product.
  Both of this section's own checks have already rotted this way in a single
  day (`d8f4e7714`, `0daa36b0b`), each returning a believable empty result
  while the thing it measured was working perfectly. **A missing record is a
  question, never an answer.**

### 4.4 The Pi panes

```bash
tmux -L <socket> capture-pane -p -S -100 -t <pane-id>
```

**Any repeating error line in a person's pane is a fault**, and one shape has
shipped to a live company exactly as written:

```
Extension "<runtime>" error: Agent is already processing a prompt
```

repeating. Capture the pane, the person, and the timestamps of the first and
last repetition.

### 4.5 tmux authority tags

```bash
tmux -L <socket> show-options -p -t <pane-id>
```

The tags that decide ownership:

| Tag | Means |
|---|---|
| `@organization_person_id` | This pane belongs to that person. The exact-match check for case 6. |
| `@organization_launch_hash` | The launch this pane was minted for. A change means a different launch, not a restart. |
| `@chief_sleeping_person` | This pane is a sleeping card for that person. |
| `@chief_waking_person` | A wake is in flight for that person in this pane. |
| `@chief_wake_claim` | The claim token fencing that wake. |
| `@chief_asleep_for` | This pane is the generic sleeping notice for that unit. |
| `@organization_sidebar` | This pane is the rail. Never a person, never a body. |

**A PRESENT-BUT-EMPTY tag is not an absent tag.** `show-options -p` prints
`@chief_wake_claim` with no value when the tag exists and is empty, and that is a
different state from the tag not being listed at all — empty local ownership
fails *closed* on purpose, and reading it as absent is how an observer once
reaped live sessions. When you report a tag, report which of the three it was:
absent, present-and-empty, or present with a value.

### 4.6 Pane inventory, before and after every gesture

```bash
tmux -L <socket> list-panes -a -F \
  '#{session_name} #{window_index} #{pane_id} #{pane_pid} #{pane_width}x#{pane_height} #{pane_current_command}'
```

Diff the before and after. **Any of these is a failure:**

- a pane that appeared and is not the one the case expected,
- a duplicate pane for one person,
- a pane that died,
- a **1-column pane** (tmux's transient state while a rail shrinks; it must
  never be visible in a settled frame),
- a **rail whose width changed** — rail width is human-owned and no gesture in
  this suite may rewrite it.

To capture a plan when the round line calls for one:

```bash
cd $DIR && chief topology
```

`chief topology` prints where this client *would* place every desired person and
starts nothing. It is safe to run at any point in the suite.

### 4.7 Screenshots

Capture at **~200 ms, 1 s, and 2 s** after every click, with UTC in the
filename:

```bash
playwright-cli -s=desktop screenshot --filename=.playwright-cli/<case>-<t>-$(date -u +%H%M%SZ).png
```

The three-shot cadence exists because several of the bugs in this repo's history
were *intermediate frames*: a notice-plus-person split, a generic body, a
one-column rail — each visible for well under a second and gone by the time a
single screenshot fired.

---

### 4.8 HOW TO PROVOKE A CRASH LOOP — the staging cases 13, 19 and 35 need

**Read this before running any case whose preconditions say "a person who is
crash-looping".** Several cases need one, none of them used to say how to get
one, and two consecutive runs reported them unrunnable because waiting for a
crash loop to appear on its own does not work. It was provoked live on
2026-08-19 and this is the recipe.

**THERE IS NO LONGER ANY SUCH THING AS A HOLD.** Until 2026-08-19 five
consecutive failed boots made the actuator STOP TRYING, drop the person from
placement, and publish that verdict to `@chief_actuator_crash_holds` so no
replacement actuator would retry either. That wedged a live company for ninety
minutes after the fault causing it had cleared. The give-up is deleted: a person
chiefd wants up is retried for ever, on a backoff of 0.5s → 1s → 2s → 4s → 8s →
10s → 10s → …, and the screen carries the retry count, the elapsed time and the
last error. Everywhere this document used to say *hold*, read *crash loop*, and
everywhere it used to say *released*, read *stops on its own when the person
stays up*.

**First, the rule the mechanism imposes, because it rules out almost everything
you will think of.** `CrashLoop::observed` (`crash_loop.rs`) counts a failed
boot only two ways, and both need a pane that really existed: the person was
spawned by the PREVIOUS pass and has no pane at this one, or the person is
observed holding a DIFFERENT pane than last pass. Both are suppressed outright
when the previous pass fail-stopped. So:

* **the process must die before the next pass**, about one second. A person who
  survives one pass and dies later never accumulates — the respawn is a first
  sighting with nothing to compare against, so the count RESETS rather than
  climbing. Hand-timed kills do not work.
* **the PASS must SUCCEED.** Anything that breaks the launch instead of the
  process is deliberately not counted. This is why a gate refusal cannot
  produce a hold — see the table below — and why a bad `--session` cannot
  either (it fails `ClaimWakingFocus`, not Pi).

**GATE REFUSAL vs CRASH LOOP.** Runners keep conflating these. They are
different states with different causes and only one of them has a counter:

| | Gate refusal | Crash loop |
|---|---|---|
| Cause | `read_materialized_resources_for_launch` returns `None`: the agent home is not a directory, or neither `auth.json` nor `models.json` resolves. Plus `identity_launch_refusals`: an unusable key, or one whose fingerprint conflicts with the enrolment. | Pi starts and exits within a second, repeatedly. |
| Pane | **never created** | created every pass, dies every pass |
| Counter | never touched — the person is never in `spawning()`/`pending` and never in `observed`, so `reports()` can never contain them | climbs with every failure and never stops |
| Rail | `refused`, magenta, barred circle (Case 19) | `crashing`, amber, filled ring, with the retry number |

`rm -rf $DIR/.chief/agent/<person> && touch $DIR/.chief/agent/<person>` produces
the FIRST column, not the second. That is Case 19's and Case 23's staging and it
will never yield a hold.

**What Pi does and does not die from.** Measured on real `pi` 0.80.10 in a real
tmux pane, because guessing here wastes a run:

| Input | Result |
|---|---|
| malformed `models.json` | **stays up**, prints an in-pane error, falls back to its default |
| `--tools <name Pi does not know>` | **stays up** |
| a `--session` file that is not a transcript | exits 1 at once — but see the warning below |
| **an extension that throws at load** | **exits 1 at once, and the PASS SUCCEEDS** |

Only the last one produces a hold.

**THE RECIPE.** Every step is an ordinary operator gesture through the visible
browser. Nothing is faked: no tmux tag is edited, no store row is written, no
binary is corrupted, and no file is placed by hand.

1. Wake one person from the rail and wait for their pane.
2. Type into **their own pane**, as the operator, an ordinary engineering
   request — write yourself a small Pi extension at
   `$PI_CODING_AGENT_DIR/.pi/extensions/<name>.ts` that reads a REQUIRED
   environment variable at load time and throws a clear `Error` naming it when
   it is absent, so a misconfigured install fails loudly. This is the exact
   pattern `organization-intercom.ts` itself uses (`requiredEnvironment`), so
   the person writes correct, idiomatic code. Name a variable nothing sets.
3. Let them idle-park on their own (about two minutes), then wake them again
   from the rail.

Pi auto-discovers project extensions from its cwd, the module throws at load,
and the pane is gone in under a second — every pass. The round line reads
`applied N step(s)` each time, NOT a pass failure, so the registry counts. After
a few passes the round line carries, on EVERY round while it is true:

```
<key>: 'dana' has failed to stay up 7 times in a row over 1m 4s; retrying in 10s
and for as long as chiefd wants them up. Last error: <what the actuator saw>
```

The number climbs and never stops climbing. That is the whole behaviour: there
is no limit to wait for and nothing to release.

**To END the crash loop**, move the extension aside. Nothing else is needed and
nothing else is allowed to be needed — the person comes back up on their own
within ten seconds, and their count disappears with them. This is Case 35's
second half, and the reason it is the half that matters is that the owner's box
could NOT do it: the fault cleared and the company stayed down.

**TOMBSTONE, 2026-08-19: a held person used to make the round line print §4.2's
worst failure signature**, `this plan asked for NOTHING, so the K missing will
not be started by it`, once a second, for ever — because a held person was
stripped from placement and the plan really did ask for nothing. There is no
hold any more and no such steady state. A crash-looping person is IN the plan on
every pass whose backoff has elapsed, so that line means what §4.2 says it means
again: **it is a failure, always.** If you see it beside a `crashing` person,
file it.

---

## 5. Driving the terminal in the browser

### 5.0 BEFORE YOU TYPE ANYTHING — click inside the body pane

**After a company boots, keystrokes go nowhere until you click INSIDE THE BODY
PANE.** Focusing the xterm textarea is not enough, and the browser will tell you
everything is fine while it happens.

Verified state at the moment of failure: `document.hasFocus()` was **true**,
`document.activeElement` **was** the xterm textarea, and two full messages typed
at 45 ms delay vanished with **no echo anywhere**. The reason is not the browser
at all — the **RAIL is the active tmux pane immediately after boot**, so tmux
routes the keys to it and it silently eats them. One click on the body pane
centre fixed it, and every keystroke landed from then on. This cost a live
runner about six minutes.

```bash
# after boot, before the first keystroke of any case
playwright-cli -s=desktop run-code "async page => {
  const b = await page.evaluate(() => {
    const el = document.querySelector('.xterm-screen') || document.body;
    const r = el.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  });
  await page.mouse.click(b.x, b.y);
}"
```

Pick a point inside the BODY pane, not the rail — on a rail-left layout that
means comfortably right of the rail's width. Then type one harmless character
and confirm it echoes before you type anything that matters.

#### The two failures that look identical, and how to tell them apart

There is a real, separate failure where the terminal drops characters because
you typed too fast. From the outside it is indistinguishable from the focus
problem, and **a runner who hits the focus problem while believing it is the
speed problem will slow their typing forever and never recover.** The
discriminator is whether ANY text appeared:

| What you see | Which failure | The fix |
| --- | --- | --- |
| **Nothing echoes at all** — no partial text, no stray characters, the line stays empty | **FOCUS.** The rail is eating the keys. | Click the body pane centre (above). Re-type from scratch. |
| Text echoes but is **garbled, reordered, or missing characters** | **SPEED.** The terminal cannot keep up. | `page.keyboard.type(text, { delay: 45 })`, or xdotool `--delay 45`. |

**"No echo at all" is never a speed problem.** Speed corrupts what arrives; it
does not stop everything from arriving. If the line is empty, do not touch the
delay — fix the focus. Record which of the two you hit in the run report, since
they have different remedies and confusing them is the expensive mistake.

### 5.1 Finding a row

The terminal is xterm.js in the DOM. Rows are not elements you can name; find
them by walking text nodes and reading `getBoundingClientRect()`:

```bash
playwright-cli -s=desktop run-code "async page => await page.evaluate(() => {
  const out = [];
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = w.nextNode(); n; n = w.nextNode()) {
    const t = (n.textContent || '').trim();
    if (!t) continue;
    const r = document.createRange(); r.selectNodeContents(n);
    const b = r.getBoundingClientRect();
    if (b.width && b.height) out.push({ t, x: b.x, y: b.y, w: b.width, h: b.height });
  }
  return out;
})"
```

Then click with `page.mouse.click(x, y)`.

### 5.2 THE CALIBRATION RULE

**Click a row's CENTRE — `y + height/2` — and re-measure immediately before
every click.**

This rule has cost hours. Two independent reasons:

1. **A row's top pixel belongs to the row above.** Clicking `y` selects the
   wrong person, and the wrong person is often a plausible one, so the run keeps
   going and every subsequent reading is against the wrong subject.
2. **The rail re-renders between actions.** Coordinates measured before the
   previous gesture are stale. The row you measured has moved.

**If a click hits the wrong row, STOP and recalibrate. Never send repeat clicks
to compensate.** A second click is a second gesture: it is its own POST, its own
pane mutation, and its own entry in every count this suite is keeping. A run
that "corrected" a misclick by clicking again has destroyed its own evidence for
that case and must restart the case from a clean state.

Verify the selection landed before you believe the case: the selected row draws
a full-width purple container (`#2E1065`/`#D8B4FE` in Dark, `#EDE7F6`/`#5B21B6`
in Light). Read the selected row's text back out of the DOM and confirm it names
the person or department you meant.

---

## 6. The cases

Each case is: **preconditions → gesture → expected → verify → failure
signature.** Run them in order. Stop at the first failure.

Record for every case, pass or fail: UTC start/end, wake-POST count before and
after, pane inventory before and after, and the screenshot paths.

---

### Case 1 — Company creation via Founder

**Preconditions.** An empty directory named exactly the intended slug, on the
target, inside tmux, interactive.

**Gesture.** `chief` with no arguments (Stage C).

**Expected.** Founder opens. Given the single instruction, it creates the
company and staffs it: a CEO, one department with a manager and several more
people, everyone left asleep.

**Verify.**

```bash
test -f $DIR/.chief/db/chief.db && echo db-ok
cd $DIR && chief ls              # the company appears, state running
curl -s -XPOST $DAEMON/v1/org/activity/read -H 'content-type: application/json' -d '{}'
```

The roster read must show every person Founder was asked for, with the requested
department and manager, and `last_desired_active` false for everyone except the
CEO.

**Failure signature.** Founder refuses to start (report whether the refusal named
tmux — `chief` refuses outside tmux by design, so that is an operator error, not
a product failure); or the company is created with a roster that does not match
the instruction; or `chief ls` reports `missing` (a registry row whose directory
is gone).

---

### Case 2 — Department counts are `active/total`

**Preconditions.** Company on the glass, most people asleep.

**Gesture.** None. Read the rail.

**Expected.** Each department row shows a live count over a total, e.g. `1/5`.

- The **numerator** is live people — tmux-backed, right now.
- The **denominator** is **non-departed direct roster members** of that
  department.
- **Sleeping people stay in the denominator.** Departed people do not appear at
  all.
- The CEO stays in Executive; child-unit people do **not** roll up into a
  parent's count.

**Verify.** Count both halves independently, do not read them off the screen
twice:

```bash
# every pane carrying a person id -- NOT yet the numerator
tmux -L <socket> list-panes -a -F '#{pane_id}' | while read p; do
  tmux -L <socket> show-options -p -t "$p" | sed -n 's/^@organization_person_id //p'; done | sort -u
# the roster: who is a non-departed direct member, and who is desired
curl -s -XPOST $DAEMON/v1/org/activity/read -H 'content-type: application/json' -d '{}'
```

**Intersect the two.** A tagged pane is not automatically a live person: a
departed or stale-but-tagged pane carries `@organization_person_id` exactly like
a live one, and counting those inflated the actuator's own round line on a live
company as recently as 2026-08-18. The numerator is *people in this department
who are both on the current roster and hold a pane*; the denominator is
*non-departed direct roster members*. Never take a raw `wc -l` of tagged panes
for either half.

**COUNT THE TWO HALVES FROM TWO DIFFERENT SOURCES, AND NEVER FROM THE RENDERED
FRACTION.** The numerator comes from tmux (`@organization_person_id` panes,
intersected with the roster); the denominator comes from the store:

```bash
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select department_id, count(*) from people
     where slug='$SLUG' and employment_state != 'departed'
     group by department_id order by department_id;"
```

Then read the rail's fraction out of the **DOM**, not off a screenshot, and
compare it against the pair you computed. A rail that derives both halves from
one source can be **self-consistently wrong** — it will render a fraction that
agrees with itself and with nothing else, and a screenshot cannot tell you that.
The independent double-count is the only reading that can.

**Two behaviours that are CORRECT and must not be "fixed" later.** Both were
validated live on 2026-08-18 and a future change that breaks either is a
regression:

* **Sleepers stay in the DENOMINATOR.** A department where everyone is asleep
  reads `0/5`, never `0/0`. A sleeping person is a person.
* **A department's people do NOT roll up into Executive.** With a CEO alone in
  Executive and five people in a child department, Executive reads `1/1` — never
  `6/6`. Child-unit headcount belongs to the child unit.

A department with 1 live of 5 must read `1/5`.

**Failure signature.** A denominator that shrinks when somebody goes to sleep
(sleeping is not departure); a numerator that counts a sleeping card or a
sleeping notice as live; a parent department whose count includes a child unit's
people.

---

### Case 3 — Short person identity

**Preconditions.** Rail visible with people disclosed.

**Gesture.** None. Read a person's two-line row, then the CEO's.

**Expected.**

| Line | Content |
|---|---|
| Primary | The roster **display name**, plus ` (manager)` when that person heads the department — `Vera`, `Evan (manager)` |
| Below | The **real roster title** |

**The rail row carries NO `@handle`.** It used to print `Vera @vera`, and that
second word was `person_short_identity` of the same name already on the row — a
pure function of it, so it could never say anything the name did not. It is gone
from every rail row, at every width, for every person. The handle is NOT retired
from the product: the person's PANE BORDER still reads `@vera · Execution Lead`
and their tmux window is still named `@vera`, because there it identifies one
person instead of repeating itself down a list.

`Team member` appears **only** when no usable title exists — that is, the roster
title is absent or merely repeats the name. The CEO — the unique root department
head — carries **`Chief Executive Officer` in full**, on new and existing
companies, even where durable roster data holds an old or abbreviated title.

**"In full" is about the VALUE, not the pixels.** At the default 26-column rail
the CEO's row renders `Chief Executive Offic…`: 23 characters cannot fit in 26
columns once the status glyph and indent are taken, so the rail ellipsises it.
That is correct behaviour and this case must not fail on it. Check the value
where it is not truncated — the pane border shows it in full — and check the
rail only for the fact that it is the ROLE being shown rather than an
abbreviation, a stale title, or the person's name. The first run reported this
conflict because the case as written was unsatisfiable at default width.

Two things must never happen, and both have:

- **A department path must never appear on the row at all.**
- **A person's name must never be shown as a fake title.**

**Verify.** Read the rendered strings out of the DOM (§5.1) and compare against
the roster read. Run these as POSITIVE checks and report the value each one
found, because a check that quietly finds nothing looks identical to a check
that passed:

1. **Collect the rail person rows.** Take the text of every person row in the
   rail, as a list. An empty list is a FAILED READ, never a pass — say so and
   stop, rather than reporting the four checks below as green over nothing.
2. **Assert each row against the roster.** Every row must equal that person's
   roster display name, plus ` (manager)` when they head the department. Report
   the pair for at least one plain member and one head.
3. **Assert NO row contains `@`.** Search the collected row text for the `@`
   character and assert the count is ZERO. Report the count you measured. Any
   non-zero count is a FAIL and the offending row is the evidence.
4. **Assert the second line is the real title.** It is the roster title, not the
   person's name and not `Team member` for somebody who has one.

**Three surfaces, and only the first one lost its handle.** A runner who finds
`@vera` somewhere on the screen and a case that says "no handle" will report a
bug that is not there, so all three are stated:

- The rail ROW carries no handle at all. That is what this case checks.
- The PANE BORDER still reads `@vera · Execution Lead`. Unchanged, deliberately.
- The tmux WINDOW is still named `@vera`. Unchanged, deliberately.

Chief also keeps full roster IDs for tmux and lifecycle authority, so the pane
border's short `@vera` and the pane's own full `@organization_person_id` tag are
deliberately different strings. Do not report that as a mismatch either.

**Failure signature.** An `@handle` on a rail person row; `Team member` on a
person who has a title; a title line carrying the person's own name; a path or
department segment on the row; the CEO's role abbreviated.

---

### Case 4 — A department row shows the DEPARTMENT

**THE EXPECTED SHAPE, BY HEADCOUNT — read this before calling a layout wrong.**
The grid is `ceil(sqrt(n))` columns, and then one rule on top of it: **a row of
one is not a row.** If the last row would hold a single person, the layout drops
a row instead, because a layout string enumerates every pane and that lone
person would be stretched to the full width of the window while their
colleagues share the row above — the "merged with everybody else" shape this
module refuses to draw.

| Live people | Rows | Shape |
|---|---|---|
| 2 | 1 | side by side |
| 3 | **1** | three across — NOT 2×2 |
| 4 | 2 | 2 over 2 |
| 5 | 2 | 3 over 2 |
| 6 | 2 | 3 over 3 |
| 9 | 3 | 3 over 3 over 3 |

The first run of this suite reported three-across as a FAIL against an expected
`ceil(sqrt(3))` = 2 columns. It is not a failure; it is the row-of-one rule.
**Three live people is the WORST headcount to test a grid with** — one row is
the correct answer, so it proves nothing either way. Test with FOUR or FIVE,
where a row and a grid are different pictures.


**Preconditions.** A department with at least two people. Run it twice: once
with at least one person up, once with everybody in it asleep.

**Gesture.** One click on the **department row body** (not its `+`/`−`).

**Expected.**

- The row **expands**, and that **department's own window** goes on the glass
  with **everybody in it who is up, side by side**.
- When nobody there is up, **that department's sleeping notice** is shown
  instead.
- **Zero** wake or start requests.
- The rail's scroll offset does not change — a rail scrolled down keeps the same
  top row and the same visible sequence.

**This regressed once.** `Action::SelectDepartment` was rewritten into a click on
that department's manager, so a row meaning "this team" put one person's card on
the glass and the team was never shown at all. **Verify you are not looking at
that:** if the glass shows exactly one person's card after a department click,
the case has FAILED even if the card is correct and pretty.

**Verify.**

```bash
BEFORE=$(wake_count)
# click
sleep 2; AFTER=$(wake_count); echo "$BEFORE -> $AFTER"   # must be equal
tmux -L <socket> list-panes -a -F '#{window_index} #{pane_id} #{pane_pid} #{pane_current_command}'
```

Every live person of that department must have a pane in the focus window. In
the all-asleep run, exactly one pane tagged `@chief_asleep_for` for that unit,
and no person panes.

**Failure signature.** `AFTER > BEFORE` (any wake POST at all); one person's card
on the glass instead of the team; a sleeping notice appearing *beside* a live
department view; the rail jumping to pin the clicked row at the top.

---

### Case 5 — Selecting a sleeping person shows a card, and does not start them

**Preconditions.** A person the rail draws with the sleeping glyph — a solid red
circle, truecolor `#FF0000`.

**Gesture.** One click on that person's row (centre — §5.2).

**Expected.** Their card appears in the permanent focus body, carrying:

- first name,
- the real roster title,
- the backend-resolved effective Pi model details,
- a visible **`Wake Up`** action,
- and `@handle` **on the card's pane border**, not inside the card body.

The first run found the handle in the border rather than the body and flagged
the case as written. The border is part of the card's frame and is where every
other surface puts the handle, so that is the requirement: the handle must be
VISIBLE with the card, and the body carries name, title and model.

And: **zero** wake POSTs, and **no new person pane**.

**Verify.**

```bash
BEFORE=$(wake_count); # click; sleep 2
AFTER=$(wake_count)                                  # must equal BEFORE
tmux -L <socket> show-options -p -t <card-pane>      # @chief_sleeping_person = that person
```

The card pane must carry `@chief_sleeping_person` and must **not** carry
`@organization_person_id`. Pane inventory unchanged except the card body itself.

**Failure signature.** Any wake POST on selection; a new pane appearing; a card
showing a generic frame, a raw title, or no model line; the model line reading as
a default rather than that person's resolved model.

---

### Case 6 — One `Wake Up` click = exactly one signed request

**Preconditions.** Case 5's card on the glass. Record the card pane's id, its
pid, the rail width, and every window's geometry.

**Gesture.** One activation of `Wake Up` — mouse click, Enter, or Space; pick
one and record which.

**Expected.** All of the following, together:

1. Exactly **one** `POST /v1/org/person/wake`.
2. The pane goes to an animated `Waking up…` state.
3. **The SAME body pane becomes the Pi pane.** Not a new pane, not a second
   pane, not the card killed and a replacement made.
4. That pane's **PID changes exactly once**.
5. Final `@organization_person_id` is **exactly** that person.
6. The waking / claim / minting tags are **cleared**:
   `@chief_waking_person`, `@chief_wake_claim`, `@chief_waking_pending_claim`,
   `@chief_sleeping_person`, `@organization_minting`.
7. **Rail width unchanged, window geometry unchanged.**
8. No extra, generic, dead, or duplicate pane anywhere on the server.

**Verify.**

```bash
BEFORE=$(wake_count); PID0=$(tmux -L <socket> display -p -t <pane> '#{pane_pid}')
# activate Wake Up; sample the pane pid every 0.5s for 30s
AFTER=$(wake_count); echo "$BEFORE -> $AFTER"        # must be exactly +1
tmux -L <socket> show-options -p -t <pane>
tmux -L <socket> list-panes -a -F '#{pane_id} #{pane_pid} #{pane_width} #{pane_current_command}'
```

**Why the pane identity matters this much.** The actuator starts a sleeper's Pi
in a private off-glass session at the loading pane's exact size, keeps the
painted loading body visible until Pi has painted cells, then swaps Pi into that
fixed cell and removes the loading process in one tmux command queue. A blank
pane, a dead pane, or a second pane means that swap did not happen atomically —
which is a real regression class here, not a cosmetic one.

**Failure signature.** Two POSTs from one activation (report the two timestamps
and whether the second carried the same claim); a PID that changes more than
once; a pane id that changes; any surviving waking/claim tag after the person is
live; a rail that moved.

---

### Case 7 — A person already starting offers no card

**Preconditions.** A person the rail draws with the **starting glyph `◌`** (a
dotted cyan circle) — desired but not yet live.

**Gesture.** One click on that person's row.

**Expected.** **No `Wake Up` card.** This is by design: they are already desired,
and a wake card would offer a second wake for a start already in flight.

**This case's failure is not the missing card.** If the person is `◌` and
**never becomes live**, that is the failure to chase. Watch the round line and
the daemon log until they either come up or the round line reports the gap.

**Verify.**

```bash
# watch the round line while they should be starting
tmux -L <socket> capture-pane -p -S -200 -t chiefd-actuator-org-$SLUG-<key6>_ | tail -30
grep -E 'reconcile actuation pass|desired=|refus' $DIR/.chief/run/daemon.log | tail -40
```

**Failure signature.** The `◌` person never becomes live and the round line
prints `NOT converged · chiefd wants N … tmux holds M`. Capture `chief topology`
and the last 200 round lines and stop — that is a planner producing no steps
while people are missing, which is the top-severity signature in this suite.

---

### Case 8 — Light and Dark

**Preconditions.** A card on the glass (case 5 or 6). Record every pane id, pid,
and width.

**Gesture.** Switch the **client's colour scheme** in the browser. The browser's
`prefers-color-scheme` drives `/run/tribes-theme`, which the renderer reads on
every draw ahead of the portable environment fallback. Go **Light → Dark →
Light**.

**Expected.** The card renders correctly in both modes — Dark
`#2E1065`/`#D8B4FE`, Light `#EDE7F6`/`#5B21B6` — and **returning to Light
changes no pane and no PID.** Running Pi sessions switch their theme pair
without a process restart. A mounted card must **repaint**, not re-mount.

**Verify.**

```bash
cat /run/tribes-theme                                  # follows the browser
tmux -L <socket> list-panes -a -F '#{pane_id} #{pane_pid}'   # identical before and after the round trip
```

Screenshot in each of the three states.

**Failure signature.** A card that keeps its old colours after the switch (a
repaint that was never scheduled); any PID change across the round trip; a
session reconstructed rather than repainted; lavender rendering as `#EEEEEE` or
deep purple as `#00005F` — that last one is a client that attached before the
`xterm*:RGB` terminal-feature rule and must **reconnect**, because tmux does not
renegotiate client features. Report it as such rather than as a colour bug.

---

### Case 9 — A stuck waking pane is reclaimed

**Preconditions.** A focus pane tagged `@chief_waking_person` **and**
`@chief_wake_claim` whose person chiefd never calls desired. In a normal run
this appears on its own after a refused or withdrawn wake; do not manufacture it
by editing tags.

**Gesture.** None. Wait and watch.

**Expected.** The pane is **reclaimed within a few rounds** — the brain counts
the consecutive rounds it watched that one exact pane and claim stay unseen and
reclaims after **three**. It must not sit saying `starting…` forever.

**Verify.**

```bash
watch -n1 "tmux -L <socket> show-options -p -t <pane>"
tmux -L <socket> capture-pane -p -S -300 -t chiefd-actuator-org-$SLUG-<key6>_
```

Count the rounds between the first observation of the stuck claim and the
reclaim.

**Failure signature.** The pane still tagged `@chief_waking_person` after more
than a handful of rounds with no process behind it. This exact shape sat on a
live company for an hour: a claim never called desired never got its `pending`
mark, so on an already-recovery-ready session it matched neither recovery case
and was refused every round, forever. Report the round count, both tags with
their exact values, the pane pid, and the refusal lines.

**Note the deliberate asymmetry:** a claim the brain has *not* watched that long
is still refused, and that refusal is **correct** — a live wake in its first
rounds is indistinguishable from an orphan, and parking on sight would kill real
wakes. Do not report an early refusal as a failure.

---

**WHAT A CRASH-LOOP HOLD ACTUALLY REQUIRES — read this before you try to stage
one.** Full recipe and evidence in §4.8; the constraint in one paragraph, because
three cases need it and each was reported unrunnable by two separate runs.
A hold needs a pane **minted successfully** whose process then dies **within about
one second**, with the **pass itself SUCCEEDING**. The one-second bound is why
waiting never works: a person who survives one pass and dies later does NOT
accumulate — their respawn is a first sighting with nothing to compare against, so
the count RESETS instead of climbing. And anything that breaks the PASS instead of
the PROCESS is suppressed on purpose (`crash_loop.rs` around the
`previous_pass_failed` arms), which is CORRECT: a fail-stopped pass moved those
panes itself, so charging them to the people is charging them for the actuator's
own abandoned work. That rules out nearly every route, because Pi is resilient to
every per-person input anybody has found. **Measured, so nobody re-tests them:** a
malformed `models.json` leaves Pi UP (it warns in-pane and falls back to its
default); a `--tools` name Pi does not know leaves Pi UP; a bad `--session` kills
Pi but breaks `ClaimWakingFocus`, so the PASS fails and nothing is counted; and
every load-time throw in `organization-intercom.ts` is over `ORG_LAUNCHER_*`
variables chiefd supplies IDENTICALLY to every person, so none of them is a
per-person lever. The one door that works is a **project extension in the person's
own `.pi/extensions/` that throws at load** — Pi exits 1 at once and the pass still
reads `applied N step(s)`.

**THIS CASE IS NOT REACHABLE ON A CURRENT BUILD, AND THAT IS THE FIX WORKING.
Do not spend a run trying to stage it.** Measured live on 2026-08-19 against
`b43e48333`.

`effects::unseen_waking_focus` is the ONLY feeder of the three-round path, and it
returns `None` whenever the pane carries `@chief_waking_pending_claim` or
`@chief_waking_desired_seen`. Every wake this binary makes sets the pending claim
in the same breath as the waking tag. Sampled three seconds after a `Wake Up`
click:

```
%31  @chief_wake_claim            10d5920394f440b88c851bd05b3038dd
     @chief_waking_pending_claim  10d5920394f440b88c851bd05b3038dd
     @chief_waking_person         theo
```

So a pane minted by this binary can never enter the unseen-reclaim path at all.
That path is now recovery for what `effects.rs` calls **"legacy waking furniture
with neither marker"** — furniture minted by a pre-fix binary. And that is
precisely the bug this case describes, stated in its own failure signature: *"a
claim never called desired never got its `pending` mark."* The mark is now always
set, so the shape cannot arise. Reaching it needs a pre-fix binary or hand-written
tags, and this case forbids the second.

**Two live attempts, both correct, neither producing the shape:**

* **A genuine withdrawn wake.** The CEO was instructed to wait 60 seconds and
  then bench Theo; Theo was woken from the rail inside that window. The CEO did
  bench him (store: `theo` benched, gone from `launch_intent`), but the wake had
  already landed and all three waking tags were gone eight seconds after the
  click. Nothing stranded.
* **A wake that CANNOT land.** A person in a crash loop (§4.8) was woken, so
  every spawn died in under a second. Sampled every four seconds for ninety
  seconds: **not one sample carried any waking tag on any pane.** With the person
  held, the focus pane carried `@chief_asleep_for __focus__` — the ordinary
  generic notice — and no pane on the server carried `@chief_waking_person`.

**RUN THIS INSTEAD, because the invariant still has a subject.** *No pane may
remain tagged `@chief_waking_person` once the wake it fences has resolved, and a
wake that can never land must leave no furniture behind.* Stage a crash-looping
person (§4.8), wake them, and sample every tag on every pane every few seconds
for a minute. **PASS: no sample carries a waking tag, and when the person is
finally held the focus body is the generic notice, not `Waking up…`.** That is
cheap, it genuinely exercises the release path, and unlike the case above it can
actually fail.

---

### Case 10 — Restart preserves the company

**Preconditions.** A company with at least one person woken (case 6), a card, and
a known layout. Record the full pane inventory, the rail widths, and the roster.

**Gesture.**

```bash
cd $DIR && chief stop
cd $DIR && chief          # bare chief: this directory has a database, so it starts and enters
```

**Expected.** The CEO, the database, the rails, the operator panes, and the
geometry all come back. `chief stop` preserves durable state and asks no
confirmation; bare `chief` in a directory with `.chief/db/chief.db` starts a
stopped company **immediately**, with no interactive question.

**Note, and state it in the report:** running processes keep the **OLD binaries**
until they are restarted. A company that was running before you copied new
binaries in Stage B is running the old `chief`/`chiefd` until this restart. If
you copied binaries mid-run, every reading before this case is against the old
build — say so explicitly rather than attributing the difference to the restart.

**Verify.**

```bash
cd $DIR && chief ls                       # running
tmux -L <socket> list-panes -a -F '#{window_index} #{pane_id} #{pane_width}x#{pane_height} #{pane_current_command}'
curl -s -XPOST $DAEMON/v1/org/activity/read -H 'content-type: application/json' -d '{}'
```

Roster identical. Rail widths identical. Window geometry identical. Pane ids and
pids **will** differ — that is the restart — but the *shape* must not.

**Failure signature.** A person missing from the roster after restart; a rail at
the default width when it had been resized; a company that comes back with a
different department structure; `chief ls` reporting `missing`.

---

### Case 11 — Second observe/plan is stable

**Preconditions.** A settled company — the round line reading `converged · N up`
with `N` matching your own pane count.

**Gesture.**

```bash
cd $DIR && chief topology > /tmp/topo-1.txt
sleep 5
cd $DIR && chief topology > /tmp/topo-2.txt
```

**Expected.** `chief topology` starts nothing, and running it twice **must not
churn PIDs or layout**. The two outputs describe the same placement.

**Verify.**

```bash
diff /tmp/topo-1.txt /tmp/topo-2.txt
tmux -L <socket> list-panes -a -F '#{pane_id} #{pane_pid} #{pane_width}'   # before and after, identical
```

**Failure signature.** Any pid change, any pane appearing or disappearing, any
rail width change, or a topology diff — a read-only verb that mutates is a
failure regardless of whether the mutation was benign.

---

### Extended cases

These are derived from `CHANGELOG.md` rather than from the 2026-08-18 live run.
Run them when the eleven above have all passed; a failure here is reported the
same way.

**Case 12 — Disclosure does not scroll-jump.** Scroll the rail down, then click a
department's `+`/`−`. Disclosure changes; the scroll offset does not. The wheel
remains the only scroll gesture.

**Case 13 — A failed start does not eat the sleeping notice.** In a department
whose people are all asleep, provoke a failed person start. **Use §4.8's recipe
— it is the only route anybody has made work, and it fakes nothing.** Put the
failing person in the department under test, select that department so its
`ASLEEP` notice is on the glass, then clear
`@chief_actuator_crash_holds` to set five more failed starts running on demand.
Do not manufacture one by corrupting a binary. The existing `ASLEEP` notice keeps its **pane, its process, and the rail
geometry** across repeated failed starts. Exactly one later converge pass removes
it, atomically, only after tmux observation proves the desired person live and
fully owned.

**WHAT A CRASH-LOOP HOLD ACTUALLY REQUIRES — read this before you try to stage
one.** Full recipe and evidence in §4.8; the constraint in one paragraph, because
three cases need it and each was reported unrunnable by two separate runs.
A hold needs a pane **minted successfully** whose process then dies **within about
one second**, with the **pass itself SUCCEEDING**. The one-second bound is why
waiting never works: a person who survives one pass and dies later does NOT
accumulate — their respawn is a first sighting with nothing to compare against, so
the count RESETS instead of climbing. And anything that breaks the PASS instead of
the PROCESS is suppressed on purpose (`crash_loop.rs` around the
`previous_pass_failed` arms), which is CORRECT: a fail-stopped pass moved those
panes itself, so charging them to the people is charging them for the actuator's
own abandoned work. That rules out nearly every route, because Pi is resilient to
every per-person input anybody has found. **Measured, so nobody re-tests them:** a
malformed `models.json` leaves Pi UP (it warns in-pane and falls back to its
default); a `--tools` name Pi does not know leaves Pi UP; a bad `--session` kills
Pi but breaks `ClaimWakingFocus`, so the PASS fails and nothing is counted; and
every load-time throw in `organization-intercom.ts` is over `ORG_LAUNCHER_*`
variables chiefd supplies IDENTICALLY to every person, so none of them is a
per-person lever. The one door that works is a **project extension in the person's
own `.pi/extensions/` that throws at load** — Pi exits 1 at once and the pass still
reads `applied N step(s)`.

**VERIFIED LIVE 2026-08-19 (`b43e48333`) — PASS, both halves.** Research, all five
people asleep, its notice in its own window with `window_active=1`, the failing
person in Research. Across five consecutive failed starts the full pane inventory
was IDENTICAL in every field — notice `%23` pid 22075 213x52 and its rail `%24`
pid 22079 26x52 both unchanged, `@chief_asleep_for research` intact, nothing
appearing, dying or duplicating. Then on repair the notice became the person in
ONE cell at ONE size: `%23` gone, `%30` pid 23509 **213x52, the same geometry**,
rail untouched, `@chief_asleep_for research` cleared, round line
`applied 2 step(s)` -> `applied 1 step(s)` -> `converged · 2 up`.

**Case 14 — Stopping a department's last live person keeps its body pane.** The
actuator converts the final person pane into the generic sleeping notice in one
tmux sequence rather than killing it first. **The rail must never become the sole
full-width pane**, the pane cell and geometry stay fixed at every window width,
and no furniture is duplicated.

**Case 15 — Idle park is real and bounded.** An explicitly started person with no
work reaches the normal two-minute idle park. Normal reconciliation admits at
most **two** idle parks per pass through a durable round-robin cursor; the rest
stay stable under backpressure. A parked person keeps identity, sessions, memory,
mailbox, workspace, model, skills, and audit history — **parked means no pane and
no compute, not a deleted person.** Confirm the roster still carries them.

The "no pane and no compute" half has its own regression case now — **Case 30**,
where it failed live for a person woken from the rail — so park somebody from a
department window here and read that case for the focus-window path.

---

### Regression cases

**Read this before running them.** Cases 16 onward are not derived from the
CHANGELOG and they are not exploratory. Each one pins a bug that **shipped, was
found live on 2026-08-18, and was fixed** — every one of them worked at some
point, broke silently, and reached an operator before anything noticed. The
operator's standing instruction is the reason this block exists: *"all these
fixes need to be added to the test suite so we can verify, so we can make sure
it doesn't break going forward."*

Two rules apply to this block and not to the eleven above. (The range was
written as "16–25" when the block ended there; it grows, so it is named by its
start.)

1. **Read the PASS criterion out of the SQLITE STORE or out of tmux, never off
   the rail.** The rail is a projection and it can render a stale, correct-looking
   answer for minutes. The store cannot. Where a case gives you a `sqlite3`
   query, that query **is** the verdict and the screenshot is only evidence.
   The database is `$DIR/.chief/db/chief.db` and every read below is read-only —
   use `sqlite3 -readonly` so a fat-fingered query can never mutate a company.
2. **A regression case that passes tells you nothing unless you know the
   assertion can fail.** Each case names the exact reading the bug produced.
   If you cannot distinguish the pass reading from the fail reading, you have
   not run the case.

Each case carries the fixing commit so a failure can be bisected without
re-deriving the history.

---

### Case 16 — The founding boot introduces itself and builds nothing

Fix: `d8f1fbd63`. **Both directions are the case.** Covering only the first one
lets the second rot, and the second one is how the first was over-corrected into
a company where nobody ever starts working.

**What the operator sees when this regresses.** They finish the Founder
conversation, the company opens, and while they are still reading the first
screen the CEO starts creating departments and hiring into them. Their words:
*"It should not do anything. The very first time, just start and let the user do
anything. Don't have it create stuff in front of you."*

**Preconditions.** A company created by Founder with a roster of **exactly one
person** and no prior transcript — i.e. Stage C run with an instruction that
creates the company and staffs nobody. This is a *different* company from the
one Case 1 creates; make it in its own directory, named for its own slug.

**Gesture.** None beyond the boot. Start the company, note UTC, and **wait 90
seconds without touching anything.**

**Expected.** The CEO pane prints a two-or-three-sentence self-introduction and
then stops. No department is created, nobody is hired, no work starts.

**Verify — from the store, before and after the wait.**

```bash
BOOT_T=$(date -u +%Y-%m-%dT%H:%M:%SZ)
Q="select (select count(*) from departments where slug='$SLUG'),
          (select count(*) from people      where slug='$SLUG'),
          (select count(*) from staffing_history where slug='$SLUG');"
sqlite3 -readonly $DIR/.chief/db/chief.db "$Q"     # immediately after boot
sleep 90
sqlite3 -readonly $DIR/.chief/db/chief.db "$Q"     # 90s later
```

**PASS is `1|1|0`, twice, unchanged.** One `departments` row — the root company
unit and nothing under it; one `people` row — the CEO genesis minted;
`staffing_history` **empty**, because nothing has been hired, benched,
transferred or appointed since the company was created. The second reading
matching the first is the whole point: a CEO that is going to build an org
starts within seconds, so an unchanged count after 90 seconds is the evidence
that it stopped rather than that it had not started yet.

Then read the pane, and read it for what is **absent**:

```bash
tmux -L $SOCK capture-pane -p -S -200 -t <ceo-pane>
```

The first turn must be an introduction. It must not contain a plan, a proposed
org chart, or a tool call.

**AND PROVE IT IS IDLE, NOT MID-TURN. This is the reading that makes the case
mean anything.** Unchanged store counts are consistent with two very different
worlds: a CEO that introduced itself and stopped, and a CEO that is still
thinking and has not written its first department yet. Screenshotting a quiet
pane cannot tell them apart, and the second one fails ninety seconds later while
your report says PASS.

The discriminator is the pane's own **spend counter**. Take two screenshots at
least 30 seconds apart, inside the 90-second window, and read the figure off
both:

```bash
playwright-cli -s=desktop screenshot --filename=.playwright-cli/case16-a-$(date -u +%H%M%SZ).png
# wait >= 30s, then
playwright-cli -s=desktop screenshot --filename=.playwright-cli/case16-b-$(date -u +%H%M%SZ).png
```

**PASS: the spend is FROZEN between the two.** A frozen counter is a pane
burning no tokens, which is a pane not thinking — that is what "stopped" means
and it is not otherwise observable. A spend that is still climbing means the
turn is still running: the case is NOT decided yet, and you must let it finish
before reading the counts, because an unchanged `staffing_history` under a live
turn proves nothing at all. Record both figures and both screenshot paths in the
report, not just the verdict.

**Now the other direction — the one that must NOT be inert.** On the company
from Case 1 (or any company with more than one person), hire one new person and
let their pane come up. A hire is equally session-less, so the discriminator is
the ROSTER, not the session.

**PASS:** the new hire's first turn gets to work — the launcher's fresh-session
message contains *"continue the next real piece of work"* and does **not**
contain *"created moments ago"*. Read it from the pane's own argv, which is the
authority for what the person was actually told:

```bash
tmux -L $SOCK list-panes -a -F '#{pane_id} #{pane_pid}'   # find the new hire's pane
tr '\0' '\n' < /proc/<pane_pid>/cmdline | grep -c 'created moments ago'   # must be 0
tr '\0' '\n' < /proc/<pane_pid>/cmdline | grep -c 'next real piece of work'  # must be 1
```

**Failure signature.** Direction one: any of the three counts above growing
during the 90-second wait — `departments` at 2+, `people` at 2+, or ANY
`staffing_history` row. Report the rows themselves
(`select * from staffing_history where slug='$SLUG';`), because *which* action
was written names what the CEO invented. Direction two: a hire on a staffed
company receiving the founding copy, which is the over-correction — a company
where every later hire comes up and waits forever for an instruction nobody
knows they owe.

---

### Case 17 — Exactly one sidebar rail, counted as PANES

Fix: `a45a728d4`. **Count rail PANES, not rail TAGS.** The bug's whole
character is that the duplicate carried no tag, so every guard in the codebase —
all of which count `@organization_sidebar` — was blind to it. A tag count of 1
is exactly what this bug produces.

**What the operator sees when this regresses.** Two identical rails in one
window, one down the left edge and one down the right, each painting the same
departments. It looks like a layout bug and it is a minting bug.

**Preconditions.** A running company with the rail on the glass. Rule 0.10 first:
confirm there is exactly ONE attached chief client, or you will manufacture the
symptom yourself and report the wrong cause.

**Gesture.** None on a settled company. To drive the mechanism, restart the
company (`chief stop` then bare `chief`) and inventory immediately — the race
this bug lives in is between the pane being created and its tag being set, so it
is a boot-time window, not a steady-state one. Repeat the restart three times;
a race that reproduces once in three is still a failure.

**Expected.** Exactly one rail pane per window that has one.

**Verify — from tmux, by PROCESS, not by tag.**

```bash
# THE VERDICT: how many panes in this window are actually running a rail
tmux -L $SOCK list-panes -a -F '#{session_name} #{window_index} #{pane_id} #{pane_pid}' \
| while read s w p pid; do
    cmd=$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null)
    case "$cmd" in *"chief sidebar"*|*"chief"*" sidebar"*) echo "$s $w $p RAIL";; esac
  done | awk '{print $1, $2}' | sort | uniq -c
```

**PASS: every window shows a count of `1`.** A count of `2` on any window is the
regression, and the reading that proves the tag guards cannot see it is the two
run side by side:

```bash
# the tagged count -- what every guard in the codebase reads
tmux -L $SOCK list-panes -a -F '#{pane_id}' | while read p; do
  tmux -L $SOCK show-options -p -t "$p" | grep -q '@organization_sidebar' && echo "$p"; done | wc -l
```

**A process count of 2 beside a tag count of 1 is the exact signature of this
bug.** Report both numbers, always, even on a pass — the pair is what makes the
next reader trust the reading. It is the same pair used to confirm the
operator's box was repaired: 5 rail processes, 5 tagged panes, one-to-one.

**Why this suite may count processes and chief itself may not.** Somebody will
eventually ask why chief does not just do what this case does. It cannot. From
inside chief the only signal tmux offers is `pane_current_command`, which reads
`chief` for the tagged rail, for sleeping-person cards, and for the actuator
alike — so "a pane running chief that is not the tagged rail" is TRUE in healthy
windows, and a guard on it would refuse working companies. A guard with false
positives is worse than the duplicate it catches; the reasoning and the accepted
residual are written up in `DECISIONS.md`. This case gets to be precise because
it reads the FULL `/proc/<pid>/cmdline`, which distinguishes `chief sidebar`
from `chief sleeping-person-card` and `chief actuate` — a discrimination
available to a test on the box and not to production code holding only tmux's
answer. **So the rule is sound as a test assertion and unsound as a product
guard, and that asymmetry is deliberate.**

**The residual, stated plainly so a failure here is read correctly.**
`a45a728d4` removed the only way to CREATE an untagged rail; it does not detect
one that already exists. A rail left untagged by an OLDER binary stays invisible
to every guard for ever and must be killed by hand. If this case finds a
duplicate on a box that has run a pre-`a45a728d4` binary, that is the residual
and not a new regression — report the binary digests (0.11) alongside, because
they are what tells the two apart.

**Failure signature.** Two rail processes in one window. Capture, in this order:
both counts above, the full pane inventory with widths, `show-options -p` for
BOTH rail panes (the untagged one is the minted duplicate), and a screenshot.
Do not kill either pane — the untagged one is the evidence.

---

### Case 18 — A request that can never succeed is not retried, does not trip the provider alert, and says so in the pane

Fix: `2239dda66`. **Both directions are the case.** A classifier that calls everything permanent is
the same bug facing the other way: a real outage then never escalates and nobody
is told the company has stopped thinking.

**What the operator sees when this regresses.** A pane retries the same request
over and over, each attempt failing identically and instantly, and then their
manager receives *"N consecutive turns ended before completion"* — a provider
reliability alert for a provider that is perfectly healthy. In the session this
was found, the request reserved ~233k output tokens against a 262k context
window: it overflowed its own limit by **31 tokens**, could never have succeeded
on any attempt, and was counted three times toward an alert about provider
quality.

**Preconditions.** A live person who can complete a normal turn — run §2.5's
smoke test first, or you cannot tell this bug from a dead provider.

**Gesture, direction one (permanent).** Provoke a context-overflow refusal: give
a person a turn whose requested output plus its prompt cannot fit its model's
window. Note UTC before the gesture.

**HOW TO DO IT CHEAPLY, AND TWO TRAPS.** Verified 2026-08-19.

* **Put the person on a small-window model first.** A person on a 1M-window model
  needs a million tokens of prompt and that is not worth buying. Type
  `/model openrouter/moonshotai/kimi-k2.6` **in the person's own pane** — model
  choice is Pi's own setting now, so this is an ordinary operator gesture and not
  a store edit. That model's endpoint limit is **262144**, the same number as the
  live incident, so the reproduction is exact.
* **TRAP 1: writing a big file and having them read it does NOT overflow on its
  own.** Pi's read tool truncates. A 1,350,000-byte file moved the context from
  6.9% to 7.6% of 262K and the turn completed normally. What overflows is the
  **output reservation**, exactly as this case says — so ask for the read, let the
  context rise a little, and the next turn overflows on the reservation.
* **TRAP 2: do not size the file to sit just under the window.** A REJECTED
  request costs nothing; an ACCEPTED one is billed. Overshooting is the cheap
  direction. The whole of this case cost **1.4 cents**, all of it the first
  successful read; the three 400s were free.

**Expected.** One failure, named, not retried, and **not counted**.

**Verify — from the durable event log, not the pane.**

```bash
BUS=$DIR/.chief/bus/events.jsonl          # the durable event trail; the pane is not it
jq -c 'select(.event=="provider-turn-failed")'       $BUS | tail -5
jq -c 'select(.event=="provider-failure-escalated")' $BUS | tail -5
```

**PASS:** the overflow appears **once**, carrying `kind: "request_too_large"` —
NOT `provider_error`, which is what filed it as an outage. `automaticRetry` is
`false`. Repeating
the gesture three times must NOT produce a `provider-failure-escalated` event
and must NOT send a reliability message to the person's manager — check the
manager's mailbox in the store, which is the durable authority:

```bash
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select count(*) from mailbox where slug='$SLUG'
     and message like '%consecutive turns ended%';"
```

**PASS is `0` after three overflow refusals.**

**MEASURED 2026-08-19 (`b43e48333`) — THE CLASSIFIER HALF PASSED, THE CARD HALF
FAILED; THE CARD HALF IS FIXED AND IS NOW THE SECOND HALF OF THIS CASE,
BELOW.** Three gestures, three overflows, from `events.jsonl`:

```
kind                request_too_large      (all three — NOT provider_error)
automaticRetry      False                  (all three)
consecutiveFailures 0                      (all three — the counter never moved)
errorMessage        400: "This endpoint's maximum context length is 262144 tokens.
                    However, you requested about 263752 tokens (15631 of text input,
                    10116 of tool input, 238005 in the output)."
provider-failure-escalated                 0
mailbox "consecutive turns ended"          0
```

Beside the live incident — 262144 window, 262175 requested, 18355 text, 10003
tool, 233817 output — the shape matches to within a few percent, and it confirms
this case's central point: the prompt is 15k and the **output reservation of 238k
is what overflows**.

**The card did not reach the operator, and that was the live bug.** The pane
shows the raw OpenRouter 400 in full, including OpenRouter's nested
`previous_errors` array, so the identical sentence repeats about fifteen times per
failure and is then cut off with `[truncated 1289 chars]`. Over the whole
scrollback: `"did not fit the model"` **0**, `"will not be retried"` **0**, raw
provider dump **36**. The cause is that the card is sent with
`deliverAs: "nextTurn"`, so it is queued behind the next turn — and once a person
is in this state every next turn overflows too, so it is queued behind a turn that
can never run. A one-shot `requestTooLargeCardShown` guard means there is exactly
one attempt and it is spent on an undeliverable delivery. **That is fixed — the
card is appended rather than sent, and the guard is re-armed by any turn that
completes. This paragraph is kept as the measurement the fix answers; run the
counts below and report against those.**

**Direction one, second half — THE PANE. Do not skip this because the event log
passed.** Fix: this case's card was built correctly and delivered nowhere for
its whole first life. It was sent with `deliverAs: "nextTurn"`, which queues a
message behind the next turn — and a person in this state overflows every next
turn, so the card was queued behind a turn that could never run. Guarded by a
one-shot flag, so there was one attempt and it was spent on a delivery that
could not land. Every event assertion above was green throughout.

**Measured on the operator's live company, 2026-08-18, three overflows in one
scrollback:**

| occurrences of | count |
| --- | --- |
| `did not fit the model` | **0** |
| `will not be retried` | **0** |
| the raw provider 400 | **36** |

Thirty-six because OpenRouter nests a `previous_errors` array, so the same
sentence repeats about fifteen times per failure before the pane cuts it with
`[truncated N chars]` — two full screens of JSON, which is exactly what the card
was written to replace.

**Verify — capture the pane, then COUNT. The counts are the check.**

```bash
tmux capture-pane -p -S -3000 -t "$PANE" > /tmp/case18-pane.txt
grep -c 'did not fit the model'                  /tmp/case18-pane.txt   # want >= 1
grep -c 'will not be retried'                    /tmp/case18-pane.txt   # want >= 1
grep -c "maximum context length is"              /tmp/case18-pane.txt   # want 0
grep -c 'previous_errors'                        /tmp/case18-pane.txt   # want 0
```

**PASS:** both explanation counts are at least `1` and both raw-dump counts are
`0`. The card must also name BOTH numbers (requested and allowed) and must say
the provider is reachable — a card that says only "the request failed" leaves
the reader checking provider health that is fine.

**PASS (the card is the right card):** the box reads *Request too large for the
context window*, NOT *Provider not configured*. Those are two different cards on
one entry type, and the renderer chooses between them by payload.

**PASS (it is said once, and said again when it matters):** three overflows in a
row produce ONE card — a card per failed turn buries the intercom traffic being
read. Then complete one normal turn and overflow again: that MUST produce a
second card. Permanent silence after the first overflow is the regression, not
the fix.

**Failure signature.** Any nonzero raw-dump count, or any zero explanation count,
is this defect — whatever the event log says. A card constructed with the right
words and delivered by a mode that cannot land looks identical, from every
surface except the pane, to no card at all.

**Gesture, direction two (transient).** Provoke a genuine transport failure — a
`503`, or a connection error against an unreachable endpoint. Three in a row.

**NOT RUN on 2026-08-19, deliberately, and here is what would provoke it.** The
run that closed direction one stopped short of this half rather than inventing a
method at the end of a long night, so the next runner starts from reasoning
rather than from an absence.

The difficulty is that the failure must reach **one person only**. Breaking the
box's network, the CA bundle, or `placeholders.env` breaks EVERY pane at once, so
it tests the escalation of a company-wide outage rather than of one person's
provider — and it also destroys every other case still to run. chief holds no
credential and no longer manages providers, so it offers no per-person lever
either.

**The one per-person lever is Pi's own model configuration**, the same lever
direction one uses to reach a 262k window. So the method is: give the person a
model whose `baseUrl` points at a host that refuses the connection, then send them
three ordinary turns. Each is a genuine transport failure — a real connection
error, not a simulated one — and the classifier sees exactly what a real outage
produces. Give that person their OWN `models.json` (replace the inherited symlink
in their agent home with a real file, the same class of gesture Case 23 sanctions
for `auth.json`), so no other person is touched, and put the symlink back
afterwards.

**PASS is unchanged:** `consecutiveFailures` climbs 1, 2, 3 across the three
`provider-turn-failed` events, `kind` is `provider_error` and NOT
`request_too_large`, a `provider-failure-escalated` event is written on the third,
and the mailbox count becomes `1`. **Do not skip this half twice.** Direction one
proves the classifier narrows; only this half proves it did not switch the alert
off, and a company that cannot reach its provider failing in silence is the worse
of the two bugs.

**PASS:** `consecutiveFailures` climbs 1, 2, 3 across the three
`provider-turn-failed` events, a `provider-failure-escalated` event is written on
the third, and the mailbox count above becomes `1`. The counter is supposed to
work; this direction is what proves the fix narrowed it rather than disabled it.

**Failure signature.** Direction one: more than one `provider-turn-failed` for a
single overflow (it was retried), or `consecutiveFailures` incrementing at all,
or any escalation reaching a manager. Direction two: three real transport
failures producing no escalation — the alert has been switched off and a company
that cannot reach its provider will now fail in silence. Report the
`errorMessage` string verbatim in both directions; the classifier reads it, so
it is the input to the decision.

---

### Case 19 — Nobody sits at `starting` forever

Fixes: `39103ceac` (the actuator's own hold) and `e9b7b0202` (the launch gate's
refusal). Thirteen people on the operator's live company showed red dots and the
word `starting` and never advanced — for twenty minutes, while every surface
chiefd owns reported a healthy company.

**What the operator sees when this regresses.** A person shows `starting…` on
the rail and never becomes anything else. Not failed, not asleep, not live —
`starting`, indefinitely, with nothing on the glass saying why and nothing they
can click that changes it.

**A rail state that can never advance is itself the bug**, independent of
whatever caused it. `starting` is a PROMISE — chiefd wants this person and
something is on its way to them. When nothing is coming, the row must stop
making it.

**TWO DIFFERENT PEOPLE REACH `starting`, THEY HAVE DIFFERENT WORDS, AND THE CASE
IS BOTH.** Covering one lets the other rot; the original brief conflated them
and only one was fixed for several hours:

| Who | Glyph | Colour (light / dark) | Fed from |
| --- | --- | --- | --- |
| Their boot keeps dying and the actuator keeps restarting them — **`crashing`** | `◉` filled ring — one motion, repeating | amber `#8a2b00` / `#ff8c2b` | `CrashLoop::reports()`, via `PersonRow.crash` |
| chiefd's launch gate declined them — **`refused`** | `⊘` barred circle — struck through, because nothing happens until somebody fixes what the gate named | magenta `#7a005e` / `#ff5fd7` | `LaunchCatalog::refusals`, via `PersonRow.refused` |
| Still coming up — **`starting`** | `◌` dotted ring — in motion | teal `#005a5a` / `#00bdbd` | desired, no pane, neither of the above |

The three are deliberately different marks AND different hues so a runner
reading the rail visually can tell a gate refusal from a crash loop at a
glance. Reporting `⊘` as `◉` is reporting the wrong bug.

**Precedence on one row, and it is asserted:** live wins outright, then
`Refused` beats `Crashing` beats `Starting`. A person whose pane comes back is
`working` whatever else was true of them.

**`crashing` IS NOT A DEAD END, and the word was chosen to say so.** It replaced
`held`, which meant the actuator had GIVEN UP after five failures. Nothing gives
up any more: the person is retried on an exponential backoff capped at ten
seconds, for as long as chiefd wants them, and the row carries the retry number,
how long it has been going on, and the last error. See §4.8 and Case 35.

**Preconditions.** Two people, one of each kind. The gate-refused one is Case
23's shape — an agent home reaching no provider. The crash-looping one arises on
its own from repeated failed boots (§4.8); do not manufacture either by editing
the store.

**THE RECIPE FOR A GENUINE GATE REFUSAL, and the one that does NOT work.** The
gate refuses a person whose agent home is not there, so take the home away and
leave something in its place that is not a directory:

```bash
rm -rf $DIR/.chief/agent/<person> && touch $DIR/.chief/agent/<person>
```

The store is untouched, so rule 0.2 and the "do not edit the store" line above
both hold. Verified on 2026-08-19: this produces
`this person has no agent home (<dir>/.chief/agent/<person>); the next hire-path
pass creates it`, carried intact into the card.

**Moving `/root/.pi/agent/{auth,models}.json` aside does NOT refuse anybody**,
and a runner who tries it first will conclude the gate is broken. Two reasons,
either one sufficient: Pi REWRITES `auth.json` on any start, so the file is back
before the gate looks; and the gate accepts EITHER file, so hiding one proves
nothing about the other. Use the recipe above.

**Gesture.** Start them. Note UTC. Watch for **three minutes**.

**Expected.** Neither is `starting` after a handful of rounds, and the refused
one carries a reason an operator can act on.

**Verify — from the actuator's rounds and the store, not the rail alone.**

```bash
tmux -L $SOCK capture-pane -p -S -400 -t "$ACT" \
  | grep -n 'REFUSED\|has failed to stay up\|no launch spec\|provider'
# THE STORE'S OWN ANSWER: a launch intent with no pane behind it IS `starting`.
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select person_id, initiator_person_id, reason, started_at
     from launch_intent where slug='$SLUG' order by started_at;"
# who actually holds a pane right now
tmux -L $SOCK list-panes -a -F '#{pane_id}' | while read p; do
  tmux -L $SOCK show-options -p -t "$p" | sed -n 's/^@organization_person_id //p'; done | sort -u
```

Subtract the second list from the first: **that difference is the set of people
sitting at `starting`**, and `started_at` says for how long. This is the reading
the rail cannot give you — the rail paints a state from an intent it has no way
to age.

**PASS**, all four:

1. Neither person's row reads `starting` three minutes on.
2. The gate-refused row reads **`refused`** (`⊘`, magenta).
3. The held row reads **`held`** (`◉`, amber).

**THE REASON IS NOT ON THE ROW — it is click-only, and that is deliberate.** The
rail is 26 columns wide and the gate's sentences name two filenames and a home
path, so a reason on the row would be an ellipsis in practice. `PersonRow.refused`
holds chiefd's sentence and the ONLY place it is drawn is the click notice.

That argument is about the 26-column RAIL and reaches no further. The card the
click opens is 68 columns wide with rows to spare, and a reason cut short THERE
is a defect — Case 32.

**State the residual when you report this case: the operator learns WHICH state
from the rail and WHY only by clicking.** That is a real limit on a rail an
operator scans without touching, and it is worth re-testing if the constraint
ever changes. Do NOT report a row without a reason as a failure — this criterion
was written the other way round for several hours and would have failed a
correct product.
4. A reason of the shape *"no launch spec for person 'x'"* is a **FAIL even
   though it is a refusal** — it names an internal lookup, not anything the
   operator owns or can repair.

**THE REFUSED PERSON IS STILL DESIRED, AND THAT IS DELIBERATE.** Do not assert
they leave the desired set — they do not, and a case that demanded it would be
asking for a design that was considered and rejected. Assert `desired == true`
**and** `state == refused`. Being wanted is exactly why the row owes the operator
a cause instead of going quiet.

**AND A CLICK ON THEM MUST SEND NO WAKE. This is the one place in the suite
where a zero wake count after a click is CORRECT.** Clicking a refused person
announces `<name> cannot start: <reason>` and POSTs nothing. Run §4.3's counters
across the click:

```bash
BEFORE="$(wake_posts)/$(wake_applied)"; <click the refused person>; sleep 2
echo "$BEFORE -> $(wake_posts)/$(wake_applied)"
```

**PASS: both counts UNCHANGED**, and the notice names the person and the reason.
Everywhere else in this suite an unmoved wake count after a click is a failure
(Case 6) — here it is the requirement. Say in the report which rule you were
applying, because reading Case 6's rule onto this click reports a correct
product as broken.

**BUT DO NOT REST THE CASE ON THAT ABSENCE. There is a POSITIVE signal — and it
is the CARD, not a log line.**

```bash
grep -c 'event="sidebar.wake.refused-by-gate"' $DIR/.chief/run/daemon.log   # ALWAYS 0
```

**THAT GREP CAN NEVER FIRE. Do not use it, and do not report its `0` as a
failure.** The sidebar is a SEPARATE PROCESS from the daemon and its tracing
never reaches `daemon.log`, so the counter reads `0` before the click and `0`
after it whatever happens — including on a run where the refusal demonstrably
rendered on the glass, measured 2026-08-19. It is the same defect §4.3 warns
about in the wake counters, one layer over: a check that returns a plausible `0`
no matter what occurred is worse than one that errors.

**Read the CARD instead, which is where the refusal actually lands.** Click the
refused person and assert all five, from the rendered card:

1. the refused person's own name,
2. their title,
3. `Model Unavailable`,
4. the gate's reason,
5. a button reading `Cannot start`.

To prove the card is carrying chiefd's sentence rather than painting something
plausible of its own, read the card process's OWN argv — the reason is an
argument to it, so this is the machine-readable copy the log line was supposed
to be:

```bash
tmux -L $SOCK list-panes -a -F '#{pane_id} #{pane_pid}' | while read pane pid; do
  tr '\0' ' ' < /proc/$pid/cmdline | grep -q sleeping-person-card && \
    echo "$pane :: $(tr '\0' ' ' < /proc/$pid/cmdline)"; done
```

Expected shape: `chief sleeping-person-card <id> <Name> <Title> unavailable
<reason>` — with the gate's full sentence as the last argument.

Then use the unmoved wake counters to corroborate the card, in that order.

**Failure signature.** Either person still `starting` after three minutes; a
`refused` row with no reason or with an internal-sounding one; a refused person
whose click POSTs a wake; or `held`/`refused` shown for somebody who
demonstrably holds a pane — liveness wins, so that is the precedence broken in
the other direction. Report the round lines, the `launch_intent` rows with their
`started_at`, both wake counts, and a screenshot. **Do not click Wake Up again**
— a second gesture overwrites the evidence of the first (Rule 0.1).

---

### Case 20 — One bad step does not stop the healthy people

Fixes: `39103ceac` (a stray pane) and `9f56f997a` (a refused person). Two
separate defects that shared one signature, which is why fixing the first did
not fix the second and the case has to drive both.

**What the operator sees when this regresses.** A company of twelve where
**nobody** comes up, because one of the twelve cannot. The historical signature
is exact:

```
the pass FAILED after 0 of 12 step(s); nothing beyond that was attempted
```

**Grep the PREFIX `the pass FAILED after `, never the literal `0 of 12`.** Which
index the bad step lands on is a function of plan ordering, not a property of
the bug: the same fault at index 3 prints `3 of 12` and a full-string match
sails past it. A fail-stop bug also hides completely when the broken step
happens to be last, which is why the preconditions ask you to run it both ways.

Read it once and the cause is obvious; read it believing the company is merely
slow and it costs an hour. The interpreter is **fail-stop by design**, so
whatever makes a step fail takes every step behind it. Two things did:

* **A stray pane the layout would not count** (`39103ceac`). Converge quarantined
  an untagged pane — correct, a stray is skipped and never killed — then built
  the window's `select-layout` string without it, against a window that still
  held it. tmux answered `have 7 panes but need 6`, and the spawn steps behind
  it were abandoned on every later pass.
* **A person the launch gate refused** (`9f56f997a`). The refusal became step
  failure number zero.

**FAIL-STOP IS NOT WEAKENED, and a case that expects it to be is wrong.** A
missed precondition, a tmux refusal, a host error, and a plan naming somebody
the catalog never iterated all still stop the pass — each means the pass is
wrong about the world. Exactly one thing is now non-fatal: a person chiefd's
gate declined, which is expected, re-derived every pass, and named in full by
the process that owns the disk.

**TELL A CORRECT FAIL-STOP FROM THIS BUG BY THE STRING, NOT BY THE FACT THAT THE
PASS FAILED.** These look identical from the round line's first clause and only
one of them is a defect:

| What you read | Verdict |
| --- | --- |
| `skipped: '<person>' cannot be launched: <reason>` | **CORRECT.** The gate declined them; the step was skipped and the pass went on. This is the fix working. |
| `person '<id>' is not in the launch roster (N people iterated)` | **CORRECT fail-stop.** Structurally different from a refusal — this person was never a candidate for lookup at all, so the pass really is wrong about the world. Report it, do not file it as this case. |
| `window '<id>' was referenced before it was created` | **CORRECT fail-stop** when the window is genuinely unbound. But if it follows a REFUSED person who would have minted that window, it is this bug: the tail behind a skipped window must skip by name, not fail. |
| `the pass FAILED after N of M step(s)` with a refusal as the cause | **THE BUG.** |

The third row is the one that will catch somebody out, because the same message
is correct and incorrect depending on what precedes it. Read the twenty lines
above it before deciding.

**Preconditions.** A company of **at least four** people where exactly ONE will
be refused and the rest are healthy.

**TWO DIFFERENT GATES PRODUCE A REFUSAL, and the skip carries only a
`(person, reason)` pair — it knows nothing about which one said no.** So if you
name a cause in the report, name which gate, and prefer running this case once
per gate:

* **The provider gate** (`b582b20b5`) — an agent home reaching neither
  `auth.json` nor `models.json`. This is Case 23's shape.
* **The identity gate** — an enrolled key that disagrees with the one in the
  person's home, so they could never authenticate.

Both land in `LaunchCatalog::refusals` and both must be skipped rather than
fatal. A fix verified against only one of them is verified against half the
surface, which is exactly how this bug survived its first fix. Run it **twice** — once
with the refused person early in the plan and once with them late — because a
fail-stop bug hides completely when the broken step happens to be last.

**Run it once more with the refused person FIRST**, specifically to drive the
window case: a refused person who would have MINTED a window takes only that
window with them, and the tail behind it skips by name rather than failing on an
unbound window id.

**Gesture.** Start the whole company. Note UTC.

**Expected.** The healthy people come up. The refused one does not, and is named.

**Verify — count who is actually live, from tmux, and cross it against the store.**

```bash
# people holding a real pane right now
tmux -L $SOCK list-panes -a -F '#{pane_id}' | while read p; do
  tmux -L $SOCK show-options -p -t "$p" | sed -n 's/^@organization_person_id //p'; done | sort -u
# who chiefd wanted
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select id from people where slug='$SLUG' and employment_state='active' order by id;"
# and the round line
tmux -L $SOCK capture-pane -p -S -400 -t "$ACT" \
  | grep -n 'REFUSED\|cannot be launched\|is not in the launch roster\|the pass FAILED after '
```

**PASS**, all four:

1. **Every active person except the refused one holds a pane.** `N-1` of `N` up
   is the pass here — a full house is not expected and one person short is not
   the bug.
2. The round line **NAMES** them with chiefd's reason:
   `· chiefd's launch gate REFUSED 1 of them, so their step was skipped and the
   rest of this plan still ran: vera (<reason>)`. Named, never counted — "2
   people were refused" only sends somebody to the log to find out who.
3. **`the pass FAILED after ` does NOT appear** for the refusal. It stays for
   genuine internal errors, so its absence here is the assertion and its
   presence is the regression.
4. **The refused person is NOT held by the crash-loop registry.** A refusal is
   not a failed boot: nothing was spawned, so no pane could die, and a hold
   earned that way would outlive the refusal that caused it. Confirm no
   `has failed to stay up` line names them.

**Failure signature.** `the pass FAILED after N of M step(s)` naming a refusal —
or any live count materially below `N-1`. Capture that line with the twenty
rounds either side, plus the live-person set showing how many healthy people
were left on the floor. Report the refused person's id and reason separately: a
refusal is correct, and only its blast radius was ever the bug.

---

### Case 21 — `chief stop` takes the actuator down with the company

Fix: `86438156c`. Found by this suite, on two companies independently.

**What the operator sees when this regresses.** Nothing, which is the problem.
`chief stop` returns cleanly, the company is gone from the glass, and an orphan
`chiefd-actuator-org-<slug>-<key6>_` session keeps running forever, retrying the
daemon that stop just killed —
`could not reach http://…/v1/org/runtime/desired … retrying in 8s/16s/30s` —
and, because a live actuator's documented job includes re-minting a company
session somebody killed, it can bring the company back after a stop that already
reported success.

**Preconditions.** A running company. Record both session names first:

```bash
CO=$(tmux -L $SOCK ls -F '#{session_name}' | grep "^org-$SLUG-"); ACT="chiefd-actuator-$CO"
tmux -L $SOCK ls -F '#{session_name}' | grep -E "^($CO|$ACT)$"    # both must be listed
```

**Gesture.** `cd $DIR && chief stop`.

**Expected.** Both sessions gone. The actuator goes down **before** the company
session, and the outcome JSON says so about both halves.

**Verify — from tmux, and from stop's own report.**

```bash
tmux -L $SOCK ls -F '#{session_name}' | grep -E "^($CO|$ACT)$" ; echo "exit=$?"   # exit=1: neither remains
sleep 20
tmux -L $SOCK ls -F '#{session_name}' | grep -E "^($CO|$ACT)$" ; echo "exit=$?"   # STILL exit=1
```

**PASS: `exit=1` both times**, and `chief stop`'s outcome JSON carries
`actuatorStopped` **beside** `sessionStopped`. The twenty-second re-read is not
belt-and-braces: a surviving actuator re-mints the company session, so a company
that reappears after a successful stop is this bug wearing a different symptom.

**Failure signature.** `$ACT` still listed; or the company session reappearing
after the wait; or an outcome JSON that reports only `sessionStopped` — a stop
that names one half of what it tore down is what let this survive unnoticed in
the first place. Capture `ps -o pid,etime,args` for the surviving actuator: its
elapsed time proves it predates the stop.

---

### Case 22 — A removed department's sleeping notice goes with it

Fix: `6ea1c5a32`, completed by `31cd2279a`. Found by this suite, and then the
FIRST fix was found still broken by running this case live — which is the whole
argument for the case existing. `6ea1c5a32` made the sweep FIND the stale
notice; it then declined to remove it, because the helper it used protects a
WATCHED window by keeping its last content pane for the next pass to replace.
A department that woke gets that next pass. A department that has been REMOVED
never does, so the courtesy became permanent.

**So run this case with the window ACTIVE.** A removed department whose window
is not the one on the glass will pass while the reported bug is fully present:
the operator's own report was a window they were looking at.

**What the operator sees when this regresses.** They remove a department from a
running company and its `ASLEEP` notice stays on the glass, in its own window,
for ever. Only a restart clears it. Nothing owns the pane, because placement is
derived from the current tree and that department is no longer in it, so no
converge pass ever looks at it.

**Preconditions.** A running company with **two** departments whose people are
all asleep, both showing their sleeping notice. Record the window inventory and
the notice panes:

```bash
tmux -L $SOCK list-panes -a -F '#{window_index} #{pane_id}' | while read w p; do
  tmux -L $SOCK show-options -p -t "$p" | sed -n "s/^@chief_asleep_for /$w $p /p"; done
```

**Gesture.** Select the department you are about to remove so **its window is
active**, then remove exactly that one, through the UI. Record
`window_active` for its window before the gesture — that flag is the condition
the first fix tripped over.

**USE A SCHEDULED REMOVAL. Instructing the CEO and racing to the window does not
work**, and it is what stopped two consecutive runs: the CEO's pane is in window
0, so instructing it puts window 0 on the glass and the sweep always wins the
race back. A removal the CEO performs on a DELAY needs no pane focus at the
moment it fires, so you can sit in the department's own window and watch. It is
the product's own mechanism, not a trick, and it keeps rule 0.10 intact — one
attached client throughout.

1. Have the CEO create a third department with two people, all asleep.
2. One message to the CEO: *"wait 120 seconds, and then remove the <name>
   department"*, and nothing else.
3. While it counts down, click the SURVIVING department's row (to draw its
   notice), then the doomed department's row, so the doomed one is active.
4. Sample the pane inventory and every `@chief_asleep_for` value repeatedly for
   at least ninety seconds after the removal lands.

**VERIFIED LIVE 2026-08-19 (`b43e48333`) — PASS.** Before, with the doomed
department WATCHED: `w3 %42` pid 787 213x52 `@chief_asleep_for archive`
`window_active=1`, its rail `%43`; survivor `w4 %44` pid 996 `@chief_asleep_for
support`. After, stable across six samples over ninety seconds: **window 3 gone
entirely**, both panes taken, the window taken WHOLE; the client moved to `w4`
which still carries its own rail, so nobody was left staring at a rail-only
window; `%44` pid 996 survives unchanged; `__focus__` survives as the by-name
exemption; no `@chief_asleep_for archive` anywhere at any sample. Store: the
removed department is gone from `departments` and its two people are
`employment_state=departed` under the executive root — the people were kept, not
lost with the unit.

**Expected.** The removed department's notice goes, and takes its window when it
was that window's last content pane. The still-sleeping department's notice
**stays** — this is not a sweep of all notices.

**Verify — the roster is the authority on which departments exist.**

```bash
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select id, name, state from departments where slug='$SLUG' order by id;"
# re-run the notice inventory above
```

**PASS:** every remaining `@chief_asleep_for` value appears in the `departments`
query; the removed one appears in neither. Cross-multiplying the two lists is
the verdict — a notice whose department is not in the roster is stale by
definition.

Two exemptions are deliberate, and reporting either as a failure is wrong:
`__focus__` is exempt **by name** (the permanent focus window parks behind it),
and a notice for a department that still exists and is merely asleep stays.

**Read this case beside Case 13, which looks like its opposite and is not.**
Case 13 says a failed start must NOT eat the notice; this case says a removed
department's notice MUST go. The discriminator is whether the department still
exists, and it is the only thing that separates them: a department that is
merely asleep — including one whose people keep failing to start — gets a later
converge pass that will replace the notice in place, so keeping it is correct. A
department that has been REMOVED never gets that pass, so keeping it is
permanent. If you find yourself unable to say which case applies, read the
roster: it is the authority, and that is why the PASS above is a set
intersection against `departments` rather than anything read off the glass.

**Failure signature.** A notice naming a department absent from the store —
measured live, at 75 seconds, as `Research — 4 people are asleep` beside a rail
that had already dropped Research, with the window still carrying
`@chief_asleep_for research` and `window_active=1`. Or — the over-correction,
and worse — the *surviving* department's notice also disappearing, which clears
every notice on the glass and is a bigger failure than keeping stale furniture
for one pass. A removed department's window is now taken WHOLE, active or not;
nothing is stranded, because every window carries its own rail and tmux moves
the client to one that still has one, so a client left staring at a rail-only
window is its own separate failure and must be reported as one.

---

### Case 23 — A person who can reach no provider is refused by name

Fix: `b582b20b5`. Found by this suite, and it is the same misdiagnosis §1.4 and
§2.5 exist to prevent — read those first.

**What the operator sees when this regresses.** A company that boots clean by
every reading available to them — tmux, the roster, the round line and the
launch catalog all report a healthy company — while every pane prints
`Error: Connection error.` and nowhere else says anything. The gate used to ask
one question, *does `<dir>/.chief/agent/<person_id>/` exist*, and a home is
symlinks by design, so a home whose `auth.json` and `models.json` **both dangle**
passed it.

**Preconditions.** A running company. Pick one person and break **only** their
provider links, on the target:

```bash
P=<person-id>; H=$DIR/.chief/agent/$P
ls -l $H/auth.json $H/models.json          # record the BEFORE state, including link targets
```

Make both links dangle by moving their targets aside — **move, do not delete**,
and put them back at the end of the case. Leave the home directory and every
other file in it exactly as it is: the home existing is precisely the condition
that used to pass.

**Gesture.** Start that person.

**Expected.** chiefd **refuses** them, by name, with a reason naming the missing
provider configuration. No pane is launched.

**Verify.**

```bash
tmux -L $SOCK capture-pane -p -S -400 -t "$ACT" | grep -i "$P"
tmux -L $SOCK list-panes -a -F '#{pane_id}' | while read p; do
  tmux -L $SOCK show-options -p -t "$p" | sed -n 's/^@organization_person_id //p'; done | grep -c "^$P$"
```

**PASS:** the pane count for `$P` is `0`, and the refusal names `$P` and says the
provider configuration does not resolve. **Then restore ONE of the two links**
(either one — `auth.json` is what a Pi sign-in writes, `models.json` is a
registry an operator may write by hand, and **either alone is enough**) and start
them again: they must now come up. Requiring both would refuse a working company,
and that direction is as much the case as the refusal is.

**Failure signature.** The person launching with both links dangling — a pane
that comes up as a Pi with no path to a model, which is worth less than no pane
at all because it looks like a working company. Or the opposite: a person with
exactly one resolving provider file being refused. Restore both links before
leaving this case, and say in the report that you did.

---

### Case 24 — The reconcile line names the desired count, and no actuation count

Fix: `0daa36b0b`. **This case exists because a green unit test said the line was
fine.** `assert_eq!(report.actuated_steps, 0)` sat in the file whose behaviour
was broken, passed on every run, and is exactly what a reader would have cited
as proof. It pinned the value's shape and never drove the thing an operator
reads. Read the LINE.

**What the operator sees when this regresses.** Every reconcile line reads
`planned=N actuated=0`, N from 1 to 8, across passes where people demonstrably
came up and the round line never once said NOT converged. Both words were wrong:
`planned=` printed `desired_people` under a name it had two designs ago, and
`actuated=` printed a field that could only ever be 0, because chiefd emits no
actions and applies none. So the one line that judges a pass reported, once a
second, that nothing was happening while everything was.

**Preconditions.** A settled company with at least one person live.

**Gesture.** None. Read the daemon log.

**Verify.**

```bash
grep -o 'desired=[0-9]*'  $DIR/.chief/run/daemon.log | tail -5
grep -c 'actuated='       $DIR/.chief/run/daemon.log
grep -c 'planned='        $DIR/.chief/run/daemon.log
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select count(*) from people where slug='$SLUG' and employment_state='active';"
```

**PASS:** `desired=` is present; `actuated=` and `planned=` are **both absent
(count 0)**; and the number after `desired=` matches the active-person count from
the store. That last cross-check is the part that has a subject — a word can be
right while the number under it is not.

**Failure signature.** Any `actuated=` at all — a field permanently zero is worse
than no field, because a reader branches on it. Or `desired=N` disagreeing with
the store's active count, which is the same class of bug under a corrected name.

---

### Case 25 — `daemon.log` stays readable

Fix: `d8f4e7714`, corrected by `e59d43042`. **The correction is half the case**
— the first fix demoted every fast 2xx, which took `POST /v1/org/person/wake`
with it, and §4.3's wake count is the instrument several cases above depend on.
A log rule that silences a mutation breaks the suite that reads it.

**What the operator sees when this regresses.** `daemon.log` at **126 MB in five
hours** on a live company: 670k lines, 653k of them `event="docstore.request"`.
On a 79 GB box at 60% full that is a real disk risk with no rotation anywhere —
but it is the worse problem for *reading* than for disk. The refusals, holds and
withheld reasons an operator opens this file for were one line in forty.

**Preconditions.** A company that has been running, quietly, for **at least 30
minutes**. A quiet company polls its own daemon several times a second for ever,
so quiet is the condition that produces the volume — do not test this on a
company two minutes old.

**Gesture.** None.

**Verify.**

```bash
ls -l   $DIR/.chief/run/daemon.log
wc -l   $DIR/.chief/run/daemon.log
grep -c 'docstore.request' $DIR/.chief/run/daemon.log
grep -c 'WARN\|ERROR'      $DIR/.chief/run/daemon.log
```

**PASS, and read the rule the right way round:** the paths the daemon POLLS
ITSELF with are DEBUG; **everything else is INFO.** The exception list names what
is silent, not what is loud — "everything except errors" is a default that gets
quieter as the product grows, and it silenced `/start`, `/hire` and `/transfer`
along with the polls. What must still be there: `>= 500` at ERROR and `>= 400` at
**WARN** — a refusal is the most operator-relevant thing this surface produces
and used to be indistinguishable from a successful poll — and anything slow stays
INFO whatever it answered.

**Then prove a MUTATION still counts, because §4.3 is built on it.** Click
`Wake Up` on one sleeping person and run §4.3's own counter across the gesture:

```bash
wake_count() { grep -c 'path=/v1/org/person/wake' "$DIR/.chief/run/daemon.log"; }
wake_count; # click; then
wake_count
```

**PASS: the count goes up by exactly one.** A count that does not move is this
regression and NOT a missed click — and it silently invalidates Case 6, which
proves one click is one wake by reading this number. Any run where the wake count
never moves must be reported as uncertifiable rather than as a wake failure.

Then prove the demotion is a demotion and not a deletion:

```bash
cd $DIR && chief stop && chief --log-level debug    # or restart the daemon at debug
grep -c 'docstore.request' $DIR/.chief/run/daemon.log
```

**PASS:** at `--log-level debug` the routine lines come back. A line that is gone
at every level was deleted, not demoted, and that is a different (worse) change
than the one this fix made.

**Failure signature.** A multi-megabyte `daemon.log` on a company that has done
nothing; a `docstore.request` count in the hundreds of thousands; a 4xx that
appears at DEBUG or not at all; or ANY state-changing route — `/person/wake`,
`/start`, `/hire`, `/transfer` — missing from the file at the default level. Report the file size, the line count, the
`docstore.request` share of it, and the box's `df -h`.

---

### Case 26 — RETIRED 2026-08-19: the record it repaired no longer exists

Was: *the obvious repair for a wedge must not leave the company worse* (fix
`ef87944aa`).

**The principle it stated OUTLIVES it, and is the reason this case is retired
rather than deleted:** *a guard that detects a problem and then permanently
refuses to make progress has converted a recoverable fault into an
unrecoverable one.* Read every guard in this product against that sentence.

**Why it is unrunnable.** The case was about the tmux session option
`@chief_actuator_crash_holds` — the record one actuator published so a
replacement would not retry the launches it had given up on — and about the
disagreement an operator caused by clearing that option by hand. The option, the
record, the re-adoption notice and the give-up they all served are deleted. A
crash-looping person is retried for ever on a bounded backoff, so there is no
verdict to publish, nothing for an operator to clear, and no wedge for a repair
to make worse.

**What replaced it:** Case 35, which asserts the same principle at its source —
the fault clears and the company comes back up with no operator gesture at all.
Its own tombstone is `crash_loop.rs`'s module doc and `resident.rs`'s
`placement_hashes` tombstone, which forbids subtracting anybody from chiefd's
desired placement again.

### Case 29 — A company that claims the shared socket starts, and moves onto its own

Fix: `8ff573ff6`. Found on the operator's own company, on the first upgrade
after `cb63690a0`.

**What the operator saw when this regressed.** One line, and the wrong one:

```text
$ chief
chiefd for /root/workspace (pid 21233) did not become healthy within 15s
```

The real reason was in `daemon.log` and nowhere else — `refusing to run company
'<key>' on runtime socket '<key>': its live runtime-ownership claim names socket
'default'` — and the two recoveries it offered were a flag to retype and
"release the claim first" with no verb named. `cb63690a0` moved `chief`'s last
socket tier off the shared `default` server and onto the company key, so from
that commit every company created BEFORE it holds a claim its client
contradicts. The refusal is correct and must stay: actuating on a server the
claim does not name converges a second, shadow fleet. What was missing was any
path from the old state to the new one.

**This case FABRICATES the old state, and it is the only case in this suite that
WRITES to a company store.** There is no other way to produce a
pre-`cb63690a0` company on a box that Stage ZERO started from nothing. The write
is one `UPDATE` of one row, on the company THIS RUN created, while it is
stopped. It does not relax rule 0.2 — never do this to a company you did not
create.

**Read rule 0.10 again before part B.** Part B puts a decoy session on the
SHARED `default` server, which is the server every bare `tmux` on the box uses.
Kill it by its exact name and nothing else, and do not run part B at all if
anybody else is on the target.

**Preconditions.** A running company from an earlier case. Capture its identity
and its CURRENT socket, which is its own key, not `default`:

Read the socket from the company's OWN claim rather than recomputing the key:
the key is `sha256(<canonical dir>)[..12]`, and **a suite that re-derives a rule
the product owns holds a second copy of that rule, which drifts silently the
first time the derivation changes.** It is the same defect as a test that
asserts a shape without driving it — a check that agrees with its own
assumption rather than with its subject.

```bash
KEY=$(python3 -c 'import sqlite3;print(list(sqlite3.connect("file:'$DIR'/.chief/db/chief.db?mode=ro",uri=True).execute("SELECT socket FROM runtime_owner"))[0][0])')
CO=$(tmux -L $KEY ls -F '#{session_name}' | grep "^org-$SLUG-")   # org-<slug>-<key6>_
echo "$KEY / $CO"                                                  # both non-empty
[ "$KEY" = default ] && echo "STOP: this company already claims the shared socket; it is not a clean subject"
```

---

#### Part A — a claim proved dead is reconciled, and the boot proceeds

**Stage the old state.** Stop the company first: a claim rewritten under a
running company is a fabrication of a state that has never existed, and part B
covers the running case honestly.

**The `UPDATE` is what makes the fixture, and the ORDER matters — `chief stop`
RELEASES the claim.** That is deliberate (`stop.rs` calls the release route as
part of teardown), and it is why "stop it and start it again" reproduces
nothing: a released claim is not a claim, the boot has nothing to adopt, it
falls straight to the company's own key, and it prints a clean pass that proves
nothing was tested. A reader staging this from a REAL pre-`cb63690a0` company
instead of from the `UPDATE` must therefore end that company UNCLEANLY — `pkill`
the daemon and `tmux kill-session -t` the company session by name — which is not
a trick but the second half of the same bug: a company killed any way other than
`chief stop` leaks its claim, and a leaked claim is the identical state.

```bash
cd $DIR && chief stop
python3 - <<PY
import sqlite3
db = sqlite3.connect("$DIR/.chief/db/chief.db")
db.execute("UPDATE runtime_owner SET status='active', socket='default', released_at=NULL")
db.commit()
print(list(db.execute("SELECT slug, status, socket FROM runtime_owner")))
PY
tmux -L default ls 2>&1 | grep -c "^org-$SLUG-" || true    # must be 0: nothing of ours is there
```

**Gesture.** `cd $DIR && chief`, in the browser (rule 0.9).

**Expected.** The company comes up. No health timeout, and nothing of this
company is ever created on `default`.

**Verify — three facts, in this order.**

```bash
# 1. The FIRST daemon boot adopted the claim rather than refusing it.
grep 'runtime socket resolved' $DIR/.chief/run/daemon.log | tail -2
#    first:  socket=default        provenance="adopted-from-runtime-owner"
#    second: socket=<KEY>          provenance="client-preference"
grep -c 'refusing to start' $DIR/.chief/run/daemon.log        # 0

# 2. The claim now names the company's OWN socket, not the shared one.
python3 -c 'import sqlite3;print(list(sqlite3.connect("file:'$DIR'/.chief/db/chief.db?mode=ro",uri=True).execute("SELECT status,socket FROM runtime_owner")))'
#    ('active', '<KEY>')

# 3. The company runs on its own server, and `default` holds nothing of ours.
tmux -L $KEY ls -F '#{session_name}' | grep "^org-$SLUG-"     # present
tmux -L default ls 2>&1 | grep "^org-$SLUG-" ; echo "exit=$?" # exit=1
```

**PASS: all three.** The two provenance lines are the whole mechanism —
`adopted-from-runtime-owner` proves the daemon stopped treating the client's
guess as an operator's demand, and `client-preference` on the boot after it
proves the claim was released and the company moved.

**Failure signature.** The health timeout itself, with `refusing to start` in
`daemon.log`: the adoption tier is unreachable again. Or a first line reading
`provenance="demanded"` — the client is passing its guess as a demand, which is
the original defect. Or `runtime_owner` still naming `default` while a session
exists on `$KEY`: the company is actuating where its claim does not point, which
is the shadow-fleet state and is WORSE than the timeout — stop the run and
report it as a mutation, per rule 0.1.

---

#### Part B — a claim naming a LIVE server is obeyed, not taken

The invariant the refusal exists for. A proven absence reconciles; nothing else
does.

**Stage.** Stop the company, restore the old claim exactly as in part A, and
then put a session for THIS company on `default` so the claim is true:

```bash
cd $DIR && chief stop
python3 - <<PY
import sqlite3
db = sqlite3.connect("$DIR/.chief/db/chief.db")
db.execute("UPDATE runtime_owner SET status='active', socket='default', released_at=NULL")
db.commit()
PY
tmux -L default new-session -d -s "$CO"      # the decoy: same name the probe asks about
tmux -L default has-session -t "$CO"; echo "staged=$?"    # staged=0
```

**Gesture.** `cd $DIR && chief`, in the browser.

**Expected.** The claim stands. The company is brought up on `default`, where
its claim says it lives, and NOTHING is created on `$KEY`.

**Verify.**

```bash
python3 -c 'import sqlite3;print(list(sqlite3.connect("file:'$DIR'/.chief/db/chief.db?mode=ro",uri=True).execute("SELECT status,socket FROM runtime_owner")))'
#    ('active', 'default')  — UNCHANGED
grep 'runtime socket resolved' $DIR/.chief/run/daemon.log | tail -1
#    socket=default  provenance="adopted-from-runtime-owner"
tmux -L $KEY ls 2>&1 | grep "^org-$SLUG-" ; echo "exit=$?"   # exit=1: nothing on the new socket
```

**PASS: the claim unchanged, and `$KEY` empty.** A boot that reads the claim's
socket as dead while a session for this company is sitting on it has converged a
second fleet, and that is the one outcome this whole path exists to prevent.

**Clean up — by exact name, and only ours.**

```bash
cd $DIR && chief stop
tmux -L default kill-session -t "$CO"        # this name only. Never `kill-server`.
tmux -L default ls 2>&1 | head -3            # whatever else is there is somebody else's
```

Then take the company back to its own socket before the next case: `cd $DIR &&
chief` reconciles it exactly as part A did, because `chief stop` released the
claim.

**Failure signature.** `runtime_owner` rewritten to `$KEY` while the decoy was
alive — the proof accepted an unproven or a live answer as absence. Capture both
servers' session lists and the daemon log; this is a mutation, so rule 0.1
applies and the run stops here.

**A third answer you may hit, and it is a PASS.** If tmux cannot answer
`has-session` at all — no server, a socket this user cannot reach — the claim is
obeyed exactly as in part B. An unproven answer is not an absence. If you see
the company stay on `default` with no decoy present, check whether tmux answered
before reporting part A as failed.

---

### Case 30 — A parked person in the focus window really loses their pane

Fix: `107b9bd34`.

**State the trap first, because it is what let this run for eight minutes in
front of somebody who was watching.** *The actuator's round line cannot see this
bug, by construction.* `converged · N up` counts **how many people chiefd
DESIRES have a pane**. A parked person is by definition not desired, so their
leaked pane is not in the numerator and not in the denominator — it is not in
the sentence at all. The round line read `converged · 1 up`, correctly, 277
times, while a Pi process ran on. **Read `list-panes`. Never accept the round
line as the answer to this question.**

**What the operator sees when this regresses.** They click a sleeping person and
click Wake Up. The person answers, goes quiet, and chiefd parks them exactly on
schedule — the store shows the park, the roster shows them asleep, the rail may
even show the department count dropping. And the process never dies. It holds a
pane and a pid for as long as the company runs. Nothing on any surface says so.

**Why it is severe rather than obscure: the focus pane is where every person an
operator wakes BY CLICKING lands.** People placed into department windows parked
and reaped correctly the whole time. The one path that leaked is the only path a
human drives by hand.

The mechanism: the actuator held an operator-view lease (`retain_focused_observed_person`,
deleted here). When chiefd withdrew the person the operator had SELECTED, the
actuator put them back into its own placement, so the converge plan saw no
difference and computed no kill. Its stated bound — "chiefd is authoritative
again as soon as focus leaves" — could never arrive, because the only thing that
moves the rail's selection off somebody is that person losing their pane, and
the lease was what kept the pane. A bound that can only be released by a signal
the loop suppresses is not a bound.

**`applied_at` DOES NOT DISCRIMINATE — do not use it.** A routine idle park is
minted `status='forced'` with `applied_at` NULL and keeps its
`active_transition_id` for ever, deliberately (`new_transition`,
`chiefd-core/src/store/activity.rs`). The live evidence says the same: three
parks that DID reap their panes also carried `applied_at=(EMPTY)`. An empty
`applied_at` is the designed steady state of a park that has been decided, not a
sign that anything is stuck.

**Preconditions.** A running company with at least one non-CEO person who is not
the CEO and is currently asleep.

**Gesture.** Wake the person **FROM THE RAIL** — click their row, then Wake Up.
That is the whole precondition: clicking is what makes them the SELECTED person
and places them in `__focus__`. Do not start them with a CLI verb; that puts
them in a department window and the bug does not reproduce. Note UTC. Then leave
the selection alone and wait out the idle lease (two minutes after the agent
goes quiet).

**Expected.** The pane goes.

**Verify — from tmux, which is the only surface that can answer.**

```bash
tmux -L $SOCK list-panes -a -F \
  '#{pane_id} #{pane_pid} #{@organization_person_id} #{@organization_window_id}'
```

**PASS**, all three:

1. No pane carries `@organization_person_id = <person>`.
2. The pid that pane held is gone from `/proc`.
3. The person is still on the roster, active, with their department and title —
   Case 15's other half. Parked is not deleted.

**Failure signature.** A pane tagged for the parked person, still alive, with
`@organization_window_id = __focus__`. That tag is the discriminator and it is
worth reading even on a pass: it is the difference between the two outcomes.

**The controlled comparison, which makes this case self-checking.** Run it
twice on the same person, and change only where they sit:

| Park | Window | Outcome |
|---|---|---|
| 1 | the department's own window | pane REAPED |
| 2 | `__focus__` | pane ALIVE 34+ minutes later |

Same person id, same roster row, same company, same binaries, same actuator.
**The window tag is the only variable.** If both parks reap, this case passes
and the fix is holding. If the department-window park reaps and the `__focus__`
one does not, you have reproduced the exact defect.

**The second check, and it is the one that can go wrong in the other
direction.** The deleted lease was itself patching a real defect — a clicked
person's pane vanishing seconds after their wake settled. So also wake somebody
and simply watch: **the pane must SURVIVE.** If it dies within a pass or two of
the wake, the deletion has over-corrected. It should not: chiefd holds a quiet
woken person up through the whole idle lease on its own, because a person whose
lease is still running is a park CANDIDATE and candidacy carries the
`maintenance-backpressure` reason that keeps them active
(`a_woken_person_who_goes_quiet_is_held_up_by_chiefd_until_the_lease_expires`).
Roughly thirty seconds of survival is enough to call it.

---

### Case 28 — chiefd never calls somebody `nothing-demanded-them` while it is warning about their mail

Fix: this branch. Seam tests:
`a_benched_person_holding_mail_is_never_reported_as_nothing_demanded_them` and
`a_person_nobody_asked_for_still_reads_nothing_demanded_them`
(`chiefd-host/src/converge_apply/cycle/tests.rs`).

**State the trap first: `nothing-demanded-them` is the reason this suite is
taught to read as benign.** It is what a healthy idle company prints, constantly
— one suite runner watched it about 300 times in one night and was right to. So
when a genuinely blocked person printed it too, the field designed to tell a
blocked person from an idle one told you nothing, and greping it told you less.

**What the operator saw.** Two contradictory statements about one person, in one
converge pass, five seconds apart:

```
WARN  chiefd converge: mail demand NOT desired — these people have pending mail
      and are not being launched
      company=suite3-labs  unmet=docs-jordan (not operational: benched,
      departed, or its unit is paused)

INFO  event="reconcile.people.withheld"
      withheld=... docs-jordan[nothing-demanded-them] ...
```

The WARN is right. The INFO was false for that person, for about 100 seconds
across roughly 20 passes. Measured on that run: `mail demand NOT desired` fired
24 times and `mail wake granted launch intent` fired 0 times.

**The mechanism, because it is not a typo.** `observed_mail` is the raw set of
people holding a pending envelope. `mail_demand` is that set with every
non-operational person filtered out, and the maintenance half applies the same
filter. Only the FILTERED union reaches `activity::reconcile` as requested
demand, so the decision it returns for a benched person carries no reason at
all, and an empty reason list was rendered as `nothing-demanded-them`. The
sentence was true of the filtered input and false of the world. The WARN got it
right only because it goes back to the unfiltered set.

**The behaviour was never wrong and must not be "fixed".** A benched person is
deliberately excluded from mail demand. Do not make a benched person launch.

**How to check it on a live box.** Bench somebody who has unread mail, or
message somebody in a paused unit, and let two passes run. Then read
`daemon.log`:

```
grep 'mail demand NOT desired' <company>/.chief/logs/daemon.log
grep 'reconcile.people.withheld'  <company>/.chief/logs/daemon.log
```

Take the person id the first grep names and look for it in the second. Their
bracket must read `pending-mail-but-not-operational` — alone, or joined by `+`
to whatever reasons the decision carries of its own. **If it reads
`nothing-demanded-them`, this case has regressed**, and note that the whole
line looks healthy when it does: every other person on it is idle and correctly
says the same thing.

**The other direction, and it is the one a careless fix breaks.** People nobody
asked for must still read `nothing-demanded-them`. Run an idle company and read
the same line: an ordinary company at rest prints that reason for most of its
roster, and if it has started printing a mail reason for people with no mail,
the fix has over-corrected and every idle company now looks blocked.

`maintenance-demand-but-not-operational` is the same reason on the maintenance
half of the demand. No WARN exists for that half, so it has never been seen on a
live box; it is listed here so it is not read as noise if it appears.

---

### Case 31 — A manager sets the company's model, everyone actually switches, and it survives a restart

Fix: this batch. **Run all three parts in order.** They are one operator gesture end
to end, and each part passes while the next one is broken — a persistence-only
check goes green while nobody switched at all, and a switch-only check goes green
while every restart throws the answer away.

**The rules being tested, confirmed by the operator in their own words:**

1. A manager changes its own model, or everyone in its subtree. *"Every manager
   should be able to tell everybody to set their model, right? And they should
   all probably get set."*
2. A human typing `/model` in any pane works, and sticks.
3. A human telling any person "change your model" works, and sticks.
4. **Nothing else touches it. Ever.** *"there is no other code out there that
   resets it or defaults it."*
5. Pi owns persistence. *"just let it write to Pi the way Pi files that it
   usually does, and when you open again it reads it back."*

**Before you start, read the two settings files. They decide what part 31c means:**

```bash
cat $DIR/.pi/settings.json 2>/dev/null | grep -n 'defaultModel\|defaultProvider\|defaultThinkingLevel'
cat ~/.pi/agent/settings.json | grep -n 'defaultModel\|defaultProvider'
ls -t $DIR/.chief/agent/*/sessions/*/*.jsonl | head -1 | xargs grep -m1 -o '"modelId":"[^"]*"'
```

The third command is the model the company is actually on right now. Write all
three down; **you cannot read this case's result without them.**

#### 31a — A bad model id is refused at the ask, not 20 times at the targets

**Gesture.** In the chief pane, ask for a model that does not exist — and use a
plausible wrong spelling, not obvious junk: take the real model id and drop its
version suffix (`deepseek/deepseek-v4-flash-0731` → `deepseek/deepseek-v4-flash`).

**Expected.** One refusal, in the chief pane, naming models that DO exist.
Nothing queued. No card in anybody else's pane.

**Verify:**

```bash
# nothing was written durably
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select count(*) from maintenance_requests where slug='$SLUG' and action='set_model';"
```

**PASS:** the count is unchanged from before the gesture, and the chief's refusal
lists real model ids.
**FAIL:** the count rises. One bad guess then becomes one failed apply per person
— on the live company that produced this case it was **145 failed applies from
four bad spellings of one model**, every one of them a separate card.

**Also check the refusal's WORDING, because this is what made the defect unreadable:**

```bash
tmux -L $SOCK capture-pane -p -S -200 -t "$CHIEF_PANE" | grep -n 'no model\|cannot use'
```

**PASS:** provider and model appear as separate quoted fields —
`Pi has no model "…" from provider "…"`.
**FAIL:** anything of the form `provider/model` joined by a slash. An openrouter
id already carries a vendor namespace, so that join renders
`openrouter/openrouter/deepseek/…` and reads as a doubled provider prefix. It was
reported as a concatenation bug that does not exist, and it cost a full
misdiagnosis. **If you see the doubled form, report it as this line's failure and
not as a concatenation bug** — check the stored value before concluding anything:

```bash
sqlite3 -readonly $DIR/.chief/db/chief.db \
  "select provider, model from maintenance_request_models where slug='$SLUG';"
```

#### 31b — A good model id switches EVERYBODY, including the manager

**Gesture.** In the chief pane: *"set the model for everyone to `<a real model id
from the list 31a printed>`"*. Note UTC.

**Expected.** Every person switches — **and so does the chief**.

**Verify — the chief is the one to check, because it is the one that used to be skipped:**

```bash
for f in $(ls -t /root/.pi/agent/sessions/*/*.jsonl | head -1) \
         $(ls -t $DIR/.chief/agent/*/sessions/*/*.jsonl | head -5); do
  echo "== $f"; grep -o '"modelId":"[^"]*"' "$f" | tail -1; done
```

**PASS:** the chief's own transcript and every worker's end on the requested
model.
**FAIL:** every worker switched and the chief did not. `set_model` carried
`id !== requester.id`, so "everybody under me" was the one instruction that
skipped the person giving it — and the chief's pane is the one the operator
watches, so the whole company read as having ignored them.

**And read the cards, which is where the second defect lived:**

```bash
# The manager's pane, where the ask was made.
tmux -L $SOCK capture-pane -p -S -200 -t "$CHIEF_PANE" | grep -n 'undefined\|Model'
# And one worker's, where the settled card is written.
WORKER=$(tmux -L $SOCK list-panes -a -F '#{pane_id} #{@organization_person_id}' \
  | awk '$2!=""&&$2!="chief"{print $1; exit}')
tmux -L $SOCK capture-pane -p -S -200 -t "$WORKER" | grep -n 'undefined\|Model'
```

**TWO DIFFERENT CARDS ANSWER THIS, IN TWO DIFFERENT PANES, AND THE CASE IS
BOTH.** Reading either one's copy onto the other reports a correct product as
broken, which is what this criterion did until `card-content` measured it:

| Card | Pane | Written when |
| --- | --- | --- |
| `🔁 Model change queued · @chief, @alex, @sam` — the tool RESULT | the manager's own pane, the one you just typed in | at the ask, once, for the whole fanout |
| `🔁 Model changed · @gus` — the session-maintenance MESSAGE | each TARGET's pane | at the terminal phase only (completed / failed / skipped) |

**PASS — both, and the model id is on each:**

* In the chief pane, the queued card names the model on the line under the
  title — `<the id you asked for> (provider <its provider>)`, for example
  `deepseek/deepseek-v4-flash-0731 (provider openrouter)` — and then the
  existing note `applies to each running Pi without a private HTTP call`.
* In a worker's pane, the settled card reads `🔁 Model changed · @gus` with the
  same model line.

**PROVIDER AND MODEL ARE SEPARATE FIELDS on both, and that is the assertion —
never `provider/model` joined by a slash.** An openrouter id already carries a
vendor namespace, so the joined form renders as what looks exactly like a
doubled provider prefix; it was reported as a concatenation bug that does not
exist and cost a full misdiagnosis (see 31a's last block). The provider
`openrouter` and the id `deepseek/deepseek-v4-flash-0731` written as one
`openrouter/deepseek/deepseek-v4-flash-0731` is this line's failure.

**FAIL:**

* a queued card in the chief pane with NO model on it — the shipped card said
  only `🔁 Model change queued · @chief, @alex, @sam` and its note, so two
  managers setting two different models produced two indistinguishable
  confirmations. A confirmation that omits what you changed is not one.
* the literal word `undefined` anywhere — the operator was sent
  `🧠undefined · @gus` with `undefined` for a body. Two lookup tables had no
  row for this action and a `!` told the compiler the missing row could not be
  missing. **`undefined` on a card is never cosmetic; report it.**

**The in-progress card says `🔁 Preparing model change`, not "Queueing".** The
call can still be refused at the ask (31a), and an in-progress card must not
claim an outcome it has not reached.

#### 31c — The change survives a restart

**Gesture.** `chief stop`, then start the company again. Read the model every pane
comes up on.

**Expected — and which expectation applies is decided by the FIRST command in
this case, nothing else:**

| `$DIR/.pi/settings.json` | Expected after restart | Meaning |
| --- | --- | --- |
| no `defaultModel` key | the model from 31b, everywhere | **PASS** — Pi persisted it and nothing shadows it |
| pins a `defaultModel` | that pinned model in the CHIEF pane, the 31b model in every worker | **the Pi defect** — `issues/a-project-pinned-model-can-never-be-changed.md`. NOT a chief regression |

```bash
for f in $(ls -t /root/.pi/agent/sessions/*/*.jsonl | head -1) \
         $(ls -t $DIR/.chief/agent/*/sessions/*/*.jsonl | head -3); do
  echo "== $f"; grep -m1 -o '"modelId":"[^"]*"' "$f"; done
```

The **first** `model_change` of a session is what Pi resolved at boot.

**The control that separates the two answers in one reading.** A worker's cwd is
its own agent home, whose `.pi/settings.json` chief writes with a theme and **no
model key** — so a worker always takes the global default. Chief pane on the old
model while every worker is on the new one is the Pi shadowing, exactly. Both on
the old model is neither, and is a new finding.

**Now the part that is chief's and must hold whatever Pi does:**

```bash
cat $DIR/.chief/agent/*/.pi/settings.json
```

**PASS:** every one carries a `theme` key and nothing else.
**FAIL:** any `defaultModel`, `defaultProvider` or `defaultThinkingLevel`. A
project-scope value outranks the global scope Pi writes to, so a model pinned
there can never be changed again by anyone — that single key would hand every
worker the chief pane's bug. This is rule 4, and it is the one part of rule 5
chief owns.

**Failure signature.** A durable row for a model that does not exist; `undefined`
on any card; the chief left behind by its own instruction; a slash-joined
provider/model in any refusal; or any model key in a chief-written project
settings file. Capture the chief pane, the `maintenance_requests` rows, both
settings files, and the first and last `model_change` of the chief's transcript
and of two workers'.

**Operator workaround for the 31c Pi defect, which is not a code change:** delete
the `defaultModel`, `defaultProvider` and `defaultThinkingLevel` lines from
`$DIR/.pi/settings.json`, keeping the rest of that file.

---

### Case 27 — After a socket handoff, the company runs on ONE server and holds the claim for it

Fix: `5c54e9069`. Found on a live box, 2026-08-18, on the company case
29 part A leaves behind.

**Case 29 proves the handoff HAPPENS. This case proves the company arrives.**
They are the two halves of one boot, and the whole defect lived in the gap: the
daemon moved and the company did not. Read case 29 first — this case stages
nothing of its own and reuses that company, that `$KEY`, that `$SLUG` and that
`$DIR`.

**What was measured, on a company whose handoff had completed correctly.** Both
of case 29's provenance lines were present, six seconds apart, and the row had
been released at `03:00:57.798Z`. Then:

```text
/tmp/tmux-0/qa       chiefd-actuator-org-verifynow-labs-5eaf9a_   (actuator, converged, 1 up)
/tmp/tmux-0/default  org-verifynow-labs-5eaf9a_  2 windows        (CEO pane + both rails)
```

Two servers, one company. The daemon on the new socket, the company's PEOPLE on
`default` — the shared server every bare `tmux` lands on, the one `cb63690a0`
exists to keep companies off, and the one whose last-session-exit took eleven
panes off a live company that same day. And 2m 37s later, with the company
demonstrably up, its row still read `status='released'`: **a running company
holding no claim at all**, which is the exact state the shadow-fleet refusal
exists to make impossible. A second `chief` in that directory would have met no
claim to contradict.

Two causes, one row:

* `chief actuate` read the runtime-owner row's `socketName` and never read its
  `status`, so it adopted a socket the release had vacated seconds earlier —
  while the daemon, which does filter on status, correctly ignored it.
* Nothing re-claimed. A claim is minted only when the runtime projects or tears
  down a session, and a post-handoff boot does neither: the people come back
  from durable start intent through the converge loop.

**Preconditions.** Case 29 part A, complete and passed, with the company still
up and `$KEY`, `$SLUG`, `$DIR` and `$CO` still in the shell. Do not stop the
company — `chief stop` releases the claim, and a released claim over a stopped
company is honest, so a stop erases the subject.

**Gesture.** None. This case only reads. Everything it asks about was decided by
the boot case 29 already performed.

**Verify — four facts. The first two are the ones that were wrong.**

```bash
# 1. THE PEOPLE ARE ON THE COMPANY'S OWN SERVER, and `default` holds nothing
#    of ours. This is the fact the operator sees; the rest explain it.
tmux -L $KEY ls -F '#{session_name}' | grep "^org-$SLUG-"       # present
tmux -L default ls 2>&1 | grep "^org-$SLUG-" ; echo "exit=$?"   # exit=1

# 2. THE COMPANY HOLDS A CLAIM, and it names the server it is actually on.
python3 -c 'import sqlite3;print(list(sqlite3.connect("file:'$DIR'/.chief/db/chief.db?mode=ro",uri=True).execute("SELECT status,socket,claimed_at,released_at FROM runtime_owner")))'
#    ('active', '<KEY>', '<after the release>', ...)
#    status MUST be 'active' and socket MUST be $KEY.

# 3. The actuator and the daemon agree, which is what fact 1 is made of. Read
#    the actuator pane's own environment rather than any file: placement is
#    derived per pass and nothing on disk records it.
tmux -L $KEY list-panes -a -F '#{pane_id} #{window_name}' | grep chiefd-actuator
tmux -L $KEY show-environment -t "chiefd-actuator-$CO" ORG_LAUNCHER_RUNTIME_SOCKET 2>/dev/null \
  || tmux -L $KEY list-panes -a -F '#{pane_id}' | head -1
#    ORG_LAUNCHER_RUNTIME_SOCKET=<KEY>, never `default`.

# 4. The daemon is on the same socket, from its own last word on the subject.
grep 'runtime socket resolved' $DIR/.chief/run/daemon.log | tail -1
#    socket=<KEY>  provenance="client-preference"
```

**PASS: all four.** Facts 1 and 2 together are the whole invariant — **the
company is up, it is up on exactly one server, and it holds an active claim
naming that server.** Facts 3 and 4 are there so a failure of fact 1 says WHICH
side disagreed.

**Failure signature, and it is two different bugs.**

* A session for this company on `default` while `$KEY` also holds one, or the
  actuator's `ORG_LAUNCHER_RUNTIME_SOCKET` reading `default` while the daemon
  logs `$KEY`: the client is reading the released row's socket again. This is
  the shadow-fleet state — stop the run and report it as a mutation, per rule
  0.1.
* `status='released'` while facts 1 and 3 are clean: the company is on the right
  server but holds no claim. Nothing is visibly broken and nothing will be until
  a second `chief` runs in that directory and meets no claim to contradict, so
  this one WILL be reported as a pass by anybody who only looks at tmux. Read
  the row.

**Do not read the row through `chief`.** Every question above is asked of SQL or
of tmux on purpose: this case exists because a client's own reading of that row
was the defect.

---

### Case 32 — The click-only reason is readable in full on the card

Fixes: this case's own commit. Measured on a live refused person, on the card
Case 19 sends the operator to.

**The card is the ONLY place a gate refusal is drawn.** Case 19's residual makes
that deliberate: the rail is 26 columns and the gate's sentences name filenames
and a home path. Every word of that argument is about the rail. The card is 68
columns of inner width and holds several rows for the reason, so a reason cut
short on the CARD is the click-only design failing at the one thing it exists to
do.

**Preconditions.** One gate-refused person, exactly Case 19's — an agent home
that does not exist, so chiefd's sentence carries a full filesystem path and
runs past 68 characters. Do not shorten it to fit; the length is the case.

**Gesture.** Click the refused person on the rail. Note UTC. Read the card.

**Verify — from the card pane, row by row:**

```bash
CARD=$(tmux -L $SOCK list-panes -a -F '#{pane_id} #{@chief_sleeping_person}' \
  | awk '$2=="<the refused person id>"{print $1}')
tmux -L $SOCK capture-pane -p -t "$CARD"
```

**PASS, all three:**

1. **The whole sentence is on the card**, wrapped across as many rows as it
   needs. Reassemble the rows and compare against chiefd's own copy of it,
   which is the only machine-readable one (Case 19):

   ```bash
   grep 'event="sidebar.wake.refused-by-gate"' $DIR/.chief/run/daemon.log | tail -1
   ```

   Every word in that log line's `reason` must appear on the card, in order.
2. **A path is never cut inside itself.** `(/root/companies/<slug>/.chief/…` is
   the exact failure this case was written for: the card painted
   `this person has no agent home (/root/companies/finalcheck-labs/.ch` and
   stopped, so the operator could not read which directory to repair.
3. **Nothing is dropped in silence.** If a reason is long enough that even the
   wrapped text overruns the card, the last row ends in `…`. A card that simply
   stops mid-word is the failure whatever its length.

**FAIL:** text that stops mid-sentence with blank rows beneath it. That pairing
is the signature — the card had the room and did not use it, because a ratatui
`Paragraph` with no `wrap` draws each source line once and clips it at the
widget width.

**Do not pass this case with a SHORT reason.** A reason that fits renders
identically before and after the fix, and every existing check over this card
used one, which is why the defect lived on a card three separate cases already
read. Drive a sentence longer than the card is wide or the case has asserted
nothing.

---

### Case 33 — A person with a transcript actually resumes it, and a wake that cannot succeed refuses ONE person by name

Fixes: this case's own commit. Two stacked defects, the first of which hid the
second for the whole life of the feature.

**Session resume had never once run in production.** `latest_session()` read
`<agent home>/sessions` non-recursively and kept only entries whose own name
ended `.jsonl`. Pi writes transcripts one directory DOWN, in a directory named
after the cwd they were made in, and that directory's name has no extension — so
the filter dropped every transcript and the function answered `None` for every
person, on every pass. Nothing said so, because `None` is also the correct
answer for a person with no transcript.

**And the moment a transcript WAS selected, the wake failed for ever.** chiefd
composed its resume prompt as a multi-line string; the actuator carries that
prompt inside a tmux command line, where a newline is the command separator, so
its quoting refused the whole transaction. Measured live: `step 0
(ClaimWakingFocus) precondition missed: the launch transaction contains an
invalid newline`, once a second, nineteen consecutive rounds, no remedy on any
surface. **Fixing the directory alone would have wedged every wake of every
person who had a transcript.**

**Preconditions.** A company with at least TWO people, both up, both having had
a real turn — send each of them a message and let them answer, so each has a
transcript with content in it. Note UTC.

**Part A — the transcript is found where Pi actually put it.**

```bash
ssh root@$TARGET '
  P=<person id>
  ls -la $DIR/.chief/agent/$P/sessions/
  ls -la $DIR/.chief/agent/$P/sessions/*/
'
```

**PASS:** `sessions/` holds a DIRECTORY whose name begins and ends `--`, and the
`.jsonl` transcript is inside it. That is the layout; a flat `.jsonl` in
`sessions/` is not what Pi writes and is not what this case tests.

**Part B — the person resumes rather than starting fresh.** Kill one person's
pane outright so chiefd must replace their process:

```bash
tmux -L $SOCK list-panes -a -F '#{pane_id} #{@chief_person} #{pane_pid}'   # note both
tmux -L $SOCK kill-pane -t <that person's pane id>
```

Wait for the next converge round.

**PASS, all four:**

1. **A pane comes back for that person**, with a DIFFERENT pid, inside 30
   seconds — and the round line does NOT say `the pass FAILED`.
2. **The launch carried `--session`**, pointing INSIDE a `--…--` directory:

   ```bash
   ps -eo args | grep -F "$(tmux -L $SOCK display-message -p -t <pane> '#{pane_pid}')" | head
   grep -F 'resume' $DIR/.chief/run/daemon.log | tail -5
   ```

3. **The agent's own screen shows the transcript, and NO INJECTED SENTENCE.**
   Capture the pane and read it: the resumed person continues from what Pi
   restored. Any synthesized paragraph — one telling them they were
   interrupted, or greeting them — is a FAILURE, not continuity. Operator
   ruling: *"don't insert anything ever to anything. just boot the agent."*
4. **THE OTHER PERSON IS UNDISTURBED** — same pid as before, no respawn. A wake
   that touches somebody nobody asked about is a separate failure.

**Part C — nothing follows the session path.** The launch must END at its
`--session` argument; anything after it is a positional prompt, which is the
shape the deleted resume copy had and the shape any regrowth would take.

```bash
ps -eo args | tr '\0' ' ' | grep -F -- '--session' | head -c 2000
```

**PASS:** the session path is the last argument on the line. **FAIL:** any
further argument follows it.

**Part D — an unrepresentable launch costs one person, not the pass.** This is
Case 26's principle at this seam: a guard that detects a problem must not
permanently refuse all progress. It cannot be provoked from a product surface
once Part C holds, so verify it by READING the round line for a company where
one person is refused for any reason:

**PASS:** the refused person is NAMED on the operator's round line with chiefd's
own reason beside them, and every other person in that plan still started.
**FAIL:** `the pass FAILED after 0 of N step(s)` with nobody named — one
person's problem charged to everybody, which is Case 19's shape as well as Case
26's.

**Part E — an agent's own JSONL file does not wedge it.** The live reproduction
of this was ordinary product messaging: a person was told to "keep a small
JSON-lines log of your own status", wrote one into their sessions directory, and
never woke again. Do it deliberately:

```bash
ssh root@$TARGET '
  D=$(ls -d $DIR/.chief/agent/<person>/sessions/--*--/ | head -1)
  printf "{\"at\":\"now\",\"status\":\"working\"}\n{\"at\":\"later\",\"status\":\"idle\"}\n" > "$D/status.jsonl"
'
tmux -L $SOCK kill-pane -t <that person's pane id>
```

**PASS:** the person comes back and resumes their real transcript — the log file
is never selected, whatever its modification time. **FAIL:** the pane returns
with no history, or the round line reports a failure. Remove the planted file
afterwards; it is state the next run would inherit.

---

### Case 34 — A hard reboot leaves a company that ONE `chief` stands back up

Fixes: the operator's own box on 2026-08-19. They
hard-rebooted it while `chief` was running. This is the whole transcript of what
happened next:

```text
root@host:~/workspace# chief
have 6 panes but need 5: 6a8a,225x47,0,0{26x47,0,0,29,198x47,27,0[198x23,27,0{99x23,27,0,28,98x23,127,0,30},198x23,27,24{99x23,27,24,33,98x23,127,24,79}]}
root@host:~/workspace# chief
chief attach could not install the viewport hook set: command too long
root@host:~/workspace#
```

Their ruling, which is the acceptance criterion and not a preference: **"we
cannot have such a setup. Users don't understand this shit. they just want to
stand up their company."** And, on the shape of the fix: **"run once should
always work. why twice? If you get a mismatch like that, just boot just
`@chief`. we cannot just die like that."**

**THE BAR IS ONE INVOCATION.** If this case is run as `chief` then `chief`
again, it is testing the wrong requirement and its result means nothing. Type
`chief` ONCE. A second `chief` is a FAIL of the case, not a retry of it.

**What the operator sees when this regresses.** Bare `chief` prints a line of
tmux's own vocabulary — a layout string, a `command too long`, a "run `chief
stop` here first, then retry" — and returns them to their shell with no company.
Any tmux word on the way in is the failure signature, whatever it says.

**Preconditions, and the roster size is load-bearing.** A settled company on the
target, its people up, with at least **six departments and thirty panes**. Both
failures are functions of size and a small company will not reach either:

* The layout refusal needs the actuator to be minting panes while `chief`
  attaches, which is what a cold start after a reboot is. A one-person company
  finishes converging before the attach reads its census.
* The hook refusal is arithmetic. The install command grew about 370 bytes per
  pane and tmux 3.3a's client refuses anything over `MAX_IMSGSIZE` — measured on
  a live box: 16300 bytes accepted, 16350 `failed to send command`, 17000
  `command too long`. Measured with the real builder: 8 windows / 24 panes =
  14383 bytes, and 7 windows / 35 panes = 17130 bytes. So thirty-five panes is
  where it tips, and twenty-four is not.

Record the roster, the window list and the full pane inventory first — this case
asserts they all come back:

```bash
tmux -L <socket> list-windows -a -F '#{window_id}|#{@organization_window_id}|#{window_panes}'
tmux -L <socket> list-panes -a -F '#{window_id}|#{pane_id}|#{@organization_person_id}|#{@organization_sidebar}'
cd $DIR && chief ls
```

**Gesture — provoke the unclean shutdown for real.** Nothing here is a
simulation of a signal; every process is really killed, with no path to any
release, stop or teardown code:

```bash
# 1. Every tmux server, the daemon, the actuator, every pane process.
#    NOT `pkill -9 -x tmux`: the tmux SERVER's comm is `tmux: server`, so an
#    exact-name match never reaches it. Measured 2026-08-19 — a run that used
#    -x printed an empty survivor list, `chief` "recovered", and the server had
#    never died. It was caught only because the surviving session's creation
#    timestamp predated the kill. A kill you did not prove is not a kill.
for p in $(pgrep tmux) $(pgrep chiefd) $(pgrep -f 'chief actu') $(pgrep -f 'chief side'); do
  kill -9 "$p" 2>/dev/null
done
pkill -9 -f 'node_modules/.bin/pi'
# 2. Prove the kill landed, matched LOOSELY on purpose. All must print nothing.
pgrep -a tmux ; pgrep -a chiefd ; pgrep -af 'chief '
# 3. And ask tmux itself, which is the only answer that is not about pids.
#    It must say `no server running on /tmp/tmux-0/<socket>`.
tmux -L <socket> list-sessions
```

**Put the script in a FILE and run `bash <file>`.** A `pkill -f 'chief actuate'`
typed inside an `ssh root@box '…'` one-liner matches the remote shell's own
command line and kills the session running the test. Measured, same day.

A `kill -9` of the tmux server is the closest reachable thing to power loss: the
server dies between two of its own commands, its socket file is orphaned, the
runtime-ownership row in `.chief/db/chief.db` is left `active` naming a socket
that no longer exists, and `.chief/run/daemon.json` is left pointing at a pid
nothing owns. If the box can genuinely be rebooted instead, do that — it is
strictly better evidence — and say in the report which of the two you did.

**Then, once:**

```bash
cd $DIR && chief
```

**Expected.** `chief` attaches. The company comes back: the CEO first, and the
rest of the roster behind them as the actuator converges. Nothing tmux says
reaches the terminal.

**Verify.**

```bash
cd $DIR && chief ls                       # running
tmux -L <socket> list-windows -a -F '#{window_id}|#{@organization_window_id}|#{window_panes}'
tmux -L <socket> list-panes -a -F '#{window_id}|#{pane_id}|#{@organization_person_id}'
grep -E 'sidebar.viewport.unpublished|viewport.hook.manifest-dropped|attach.session.abandoned' \
  $DIR/.chief/logs/chiefd.jsonl
```

The roster must match what you recorded. Pane ids and pids will differ — that is
the reboot — but every person must be back.

**Three separate criteria, and report each one by itself:**

1. **One invocation.** Exactly one `chief` was typed, and it attached. Paste the
   terminal, prompt to prompt, so the count is visible rather than asserted.
2. **No tmux vocabulary.** The output contains no layout string, no `have N
   panes but need M`, no `command too long`, and no instruction to run another
   command first. A line naming a DEPARTMENT or a PERSON is the product
   speaking and is fine; a line naming a pane count is not.
3. **The company, not a husk.** Every person recorded in the preconditions is
   back, in the department they were in.

**A degraded recovery still passes criteria 1 and 3, and must be reported as
what it is.** If the projection could not be reconciled, `chief` abandons it and
stands the company up from the CEO alone — `attach.session.abandoned` in the log
is that path, and it is a PASS, because the operator is in a working company and
the actuator brings the rest back. Say in the report which of the two happened:
a clean re-entry, or the CEO-only fallback. Reporting the fallback as a clean
re-entry hides the fact that the reconciliation path did not hold.

**Measured on a live box, 2026-08-19, so the numbers are not a projection.** At
45 panes / 11 windows, on the same company seconds apart: the tree before the
fix answered `chief attach could not install the viewport hook set: command too
long` with an install of `argc=9 argv_bytes=21454`; the fixed tree seated the
operator with no output and an install of `argc=9 argv_bytes=11294`. A shim that
records argv bytes and then execs the real tmux is how to measure this — put it
first on `PATH`, never in place of tmux.

**A THIRD member of the family, found by this case rather than by the report.**
Run from a pty with no winsize (a bare `script -q -c`, which is what a scripted
runner has), the tree before the fix refused the WHOLE attach with `chief attach
could not read the operator terminal size before publication` and never reached
the hook at all. If you drive this case from a script, give `chief` a real
terminal — a detached `tmux new-session -d -x 240 -y 60 '… chief …'` is a pty
with a size, and `script` alone is not — or you will measure that refusal
instead of the one you came for.

**Failure signature.** Any tmux sentence at the prompt; a second `chief` being
needed; `chief attach: … has a ChiefD process running but unhealthy; run `chief
stop` here first, then retry`; a company that comes back missing people who were
in the recorded roster.

**Clean up.** This case leaves the company running. Stop it and remove the
directory it used if this run created it:

```bash
cd $DIR && chief stop
tmux -L <socket> kill-server 2>/dev/null
```

---

### Case 35 — Work sent to a department is DELEGATED by its manager, not done by them

Fixes: this case's own commit. The operator's report, repeated over months:
*"whenever there's a manager we should ensure that they are delegators, are not
doing work — that's the biggest issue. You send an issue to a department and
then the manager is doing all the work and not even waking up his
subordinates."*

**This case asserts the BEHAVIOUR, not the wiring.** A tree that links
correctly while the manager still opens the editor is a failed run. Read the
FAIL conditions before you start: two of them are things that look like success.

**Why it kept happening, so you know what you are watching for.** Nothing was
broken. `org_send` IS the wake — a message to a settled person grants their
launch intent and brings their pane up — so delegating a sleeping subordinate
was always one call. The manager held `org_send` and `org_roster`. What the
manager read AT THE MOMENT WORK ARRIVED was a delivery envelope byte-identical
to a worker's: *"Reply only with a needed result, precise blocker, or necessary
question."* The duty to delegate lived in an `AGENTS.md` read once at boot.

**Preconditions.** A company with a CEO and ONE department that has a manager
and at least two other people. The department's people must be ASLEEP — that is
the condition under test, because "my team is asleep" is the belief that
produced the failure. The manager may be up or down; if down, the send wakes
them first and the case still holds.

Confirm the precondition rather than assuming it, and record the answer:

```bash
ssh root@$TARGET '
  cd $DIR
  sqlite3 .chief/db/chief.db \
    "select id, kind, department_id, employment_state from people order by id;"
'
```

**Never wake the subordinates yourself to make the case work.** A subordinate
you woke is a subordinate the manager did not have to wake, and the run proves
nothing. If they are already up, put them back down with `org_bench` or restart
the company, and say in the report that you did.

**Count wakes before the gesture.** `org.person.wake.applied` DOES NOT EXIST —
it was the instrument this case was written against and no code ever emitted it,
so a run that waited for it would have recorded a FAIL on a passing product
(measured 2026-08-19). `/v1/org/person/wake` is the sidebar's Wake Up route and
a manager delegating never calls it: `org_send` wakes by granting the
recipient's launch intent, and the converge pass that grants it says so by name.
That note is the instrument:

```bash
mail_wakes() { grep -c 'mail wake granted launch intent' "$DIR/.chief/run/daemon.log"; }
BEFORE="$(mail_wakes)"; echo "$BEFORE"
```

Each granted person is named on the line, so the count is never the whole
reading — the NAME is what says the wake reached a subordinate and not the
manager:

```
reconcile actuation pass company=… applied=true desired=3 \
  notes=launching: chief, ada, milo; mail wake granted launch intent: milo
```

**Gesture.** Send ONE real piece of work to the department's manager, as the
operator would — through the CEO's pane, or by typing it into the manager's own
pane. It must be work a specialist could do and the manager could not delegate
away as trivial. Use something concrete and self-contained, e.g.:

> The checkout page returns 500 for logged-out users. Find out why and fix it.
> I need the cause and the fix by end of day.

Wait for the manager to take its turn, then read the outcome.

**PASS — all four:**

1. **The manager called `org_send`** to at least one of its own people, and the
   message names an owner, an expected output, and a deadline. Read the manager's
   pane; do not infer it from the mailbox alone.
2. **`mail wake granted launch intent` names a SUBORDINATE** on a pass after the
   gesture — not the manager, and not the CEO. That is the "not even waking up
   his subordinates" half, and it is the half that has no other evidence.
3. **A subordinate's pane came up** and holds the work in its own mailbox.
4. **The manager reported back** to whoever asked, naming who owns it.

**FAIL — any of:**

- The manager edited a file, ran a build, opened the repository, or produced the
  result itself. **This is the defect.** It is a FAIL even if the answer is
  correct and even if it also sent a message afterwards.
- No pass granted a subordinate launch intent. The manager may have written a
  mailbox row and then done the work anyway; a delegation nobody woke for is not
  a delegation.
- The manager answered *"my team is asleep"*, *"nobody is available"*, or
  *"faster to do it myself"* — in the pane or in its reply. Quote it verbatim in
  the report; that sentence is the exact belief this change exists to remove.
- The manager only asked the operator what to do. Escalating without decomposing
  is not delegating.
- **The manager benched or stopped somebody instead of sending to them.** Looks
  like management, moves no work.

**Diagnosis when it fails.** Read the delivered envelope in the manager's pane.
If the guidance it received says *"Reply only with a needed result"*, the
recipient's role did not resolve — check that the manager's kind is `head` or
`executive` in the roster and that the manifest read succeeded. If it says
*"YOU ARE A MANAGER"* and the manager did the work anyway, the copy reached it
and the model ignored it; that is a real regression of this case and worth the
whole verbatim pane.

**Cleanup.** Nothing to undo beyond the company itself.

**RUN 2026-08-19 — PASS, all four criteria, on a live box, company
`/root/companies/mgr-old` (a company created by the OLD six-skill binary), on
screen in the box's own Chromium.** Precondition recorded from the roster:
`ada|head|engineering`, `milo|worker|engineering`, `rhea|worker|engineering`,
`tom|worker|engineering`, `chief|executive|executive`, and the whole department
ASLEEP — the rail read `Engineering 0/4` with four red rings. Nobody was woken to
make the case work.

Gesture, typed into the CEO's own pane: *"Send this to Ada, Head of Engineering:
the checkout page returns 500 for logged-out users. Find out why and fix it. I
need the cause and the fix by end of day."*

1. **Ada called `org_send` to a named subordinate.** Her envelope to `milo`
   (16:28:07Z) names the owner, the expected output — *"(1) the root cause of the
   500 error, and (2) the code fix applied. Provide file paths and line numbers
   where relevant"* — the evidence required, and the deadline: *"Deadline: end of
   day. Report back to me when done."*
2. **The wake landed on a SUBORDINATE.** `reconcile actuation pass … desired=3
   notes=launching: chief, ada, milo; mail wake granted launch intent: milo` at
   16:28:08Z. Milo, not Ada, not the CEO.
3. **Milo's pane came up** — `%9 π - @milo · Engineer - milo` on the company's
   own tmux server — with the work in his own mailbox.
4. **Ada reported upward, naming the owner:** *"Routed to Milo in Engineering. He
   owns investigation and fix for the checkout-page 500 on logged-out users.
   Expect cause and fix back from him by end of day, then I'll deliver the
   summary to you."*

No FAIL condition was met: she opened no file, ran no command, produced no
result herself, never said the team was asleep, benched nobody, and did not hand
the decision back to the operator. Ada's pane then retired on its own with
`launch intent withdrawn (settled): ada` — a normal settle, not a crash.

**The instrument correction above was found by this run**, and it is the kind
this suite exists to catch: waiting for `org.person.wake.applied` would have
recorded a FAIL on a product that passed all four criteria, because that event
name is in no source file in the tree.

---

### Case 36 — Role IS the installed skill set, and a conversion swaps it

Fixes: this case's own commit. Operator: *"Sometimes a manager can become a
worker, obviously — at that point you would uninstall the management skill and
add the worker skill."* Before this, every person in a company linked the WHOLE
company skill tree, so a worker read the management skill — whose first line is
"Your primary job is to delegate" — exactly as readily as a manager did.

**Preconditions.** A company with a CEO, at least one department head, and at
least one plain worker. Note UTC.

**Part A — the company ships three skills and only three.** The LIBRARY is
`<dir>/.chief/skills`; `<dir>/.pi/skills` is not the library, it is the CEO's own
install (Part B2).

```bash
ssh root@$TARGET 'ls -1 $DIR/.chief/skills/'
```

**PASS:** exactly `manager` and `worker`. **FAIL:** any of `browser`, `fal-ai`,
`market-data`, `project-status-reporting`, `organization-management` is present,
or `founder` is present — the Founder skill is pre-company only and no person in
a company may have it.

**Part B — a manager and a worker see DIFFERENT trees.** Show the links, not a
summary of them:

```bash
ssh root@$TARGET '
  for P in <head person id> <worker person id>; do
    echo "== $P"
    ls -la $DIR/.chief/agent/$P/skills/
  done
'
```

**PASS:** the head's `skills/` holds exactly one entry, `manager`, a symlink to
`../../../skills/manager`; the worker's holds exactly one entry, `worker`.
**FAIL:** either directory holds both, holds none, or is itself a symlink — a
symlink at `skills` is the retired flat link and means this pass did not
reconcile.

Prove the skill is READABLE through the link rather than merely present:

```bash
ssh root@$TARGET 'head -3 $DIR/.chief/agent/<head person id>/skills/manager/SKILL.md'
```

**PASS:** the frontmatter reads `name: manager`.

**Part B2 — the CEO is a manager too, and this is the part most likely to be
got wrong.** The Chief has NO agent home: it is the operator's own Pi, and the
one person launched with no `PI_CODING_AGENT_DIR`, so Pi discovers its skills as
PROJECT skills from its cwd — the company directory. Measured live on
2026-08-19, before this change: a CEO's pane printed `[Skills] browser, fal-ai,
market-data, organization-management, project-status-reporting`, exactly the
contents of `<dir>/.pi/skills` and nothing else. So `<dir>/.pi/skills` IS the
CEO's role install, and it must hold the manager skill alone.

```bash
ssh root@$TARGET 'ls -la $DIR/.pi/skills/'
```

**PASS:** exactly one entry, `manager`, a symlink to `../../.chief/skills/manager`.

**And read it off the product rather than the filesystem**, which is the whole
point of this part: open the CEO's pane and read the `[Skills]` line Pi prints
at the top of the session.

**PASS:** it names `manager` and nothing else. **FAIL:** it names `worker` as
well — the CEO would then be reading "You do the work." while managing
everybody, which is this change inverted for the one person it matters most
for. It is a FAIL even though every other person's tree is correct.

**Part C — conversion swaps the install.** Appoint the worker as the head of a
new department, from the CEO's pane:

> Make <worker name> the head of a new Platform department.

Wait for the next converge round, then read the same directory again.

**PASS:** the converted person's `skills/` now holds exactly `manager`, and
`worker` is GONE — uninstalled, not shadowed. Their `AGENTS.md` role line reads
`Department head`.

**FAIL:** both are present; `worker` survives; or the directory is unchanged
after two rounds — the reconcile runs on the launch path, so a company that has
not converged since the appointment has not been given its chance. Say which.

**Part D — and back.** Hand the new department to somebody else
(`org_appoint_department_head`) or transfer the person out with `vacates`, so
they are a plain member again. **PASS:** their install returns to exactly
`worker`.

**Cleanup.** Remove the Platform department if you created it, with
`org_remove_department`. Nothing else to undo.

---

### Case 37 — A company created BEFORE this change receives the new skill tree

Fixes: this case's own commit. This is the one most likely to be quietly broken,
because everything else in the suite starts from a company created by the run.

The seed this replaced stopped dead at the existence of `<dir>/.pi/skills` —
*"Chief does not inspect it, add a newly shipped skill, restore a deleted skill,
or overwrite an edited skill."* So every company was frozen at whatever shipped
the day it was created, and none of Cases 35 or 36 would have reached one.

**This case needs a company that predates the change, and you must not fake
one.** Two honest ways to get it, in order of preference:

1. **A real one.** If the box holds a company created before this SHA that you
   are permitted to touch, use it. Record its slug, its directory, and the SHA it
   was created under.
2. **A reconstructed one.** Create a company with the PREVIOUS release binary —
   the one at `/root/.chief/bin` before you install yours — so its `.pi/skills`
   is genuinely written by the old code. Record both SHAs.

**Never** hand-write an old-looking `.pi/skills` and call it a pre-existing
company. That manufactures the precondition, and the thing under test is exactly
whether real old bytes converge. If neither route is available, report **Case 37
NOT RUN** and say which route failed and why. An honest NOT RUN is worth more
than a manufactured pass.

**Before.** Record the frozen state:

```bash
ssh root@$TARGET 'ls -1 $OLDDIR/.pi/skills/; ls -la $OLDDIR/.chief/agent/*/skills'
```

Expect the old set — `browser`, `market-data`, `organization-management` and the
rest — and a flat `skills` SYMLINK in each home, every one of them pointing at
`../../../.pi/skills`. Record it: a head and a worker resolving the same link to
the same five skills is the defect this case watches leave.

**Gesture.** Install the new binary and start that company. Nothing else: no
migration command, no reset, no `rm`.

**After.**

```bash
ssh root@$TARGET '
  ls -1 $OLDDIR/.chief/skills/
  ls -la $OLDDIR/.pi/skills/
  ls -la $OLDDIR/.chief/agent/*/skills
'
```

**PASS — all four:**

1. `.chief/skills` holds exactly `manager` and `worker`.
2. Every retired skill is GONE — from the library AND from `.pi/skills`, which
   on an old company held all five.
3. `.pi/skills` holds exactly one entry, `manager`: the CEO's install.
4. Every person's home holds a real `skills/` DIRECTORY with exactly their one
   role skill in it — the flat symlink has been replaced. Compare a head's with
   a worker's and confirm they now DIFFER.

**FAIL:** the old set survives; a home still carries the flat symlink; or the
company refuses to start. A refusal here is worse than a stale tree — read the
round line for `the company skill library could not be reconciled`, which is the
named warning this path emits rather than failing the launch.

**Cleanup.** Leave the company as you found it otherwise. If you created it in
Part 2 with the old binary, remove it with `chief rm` at the end — it is yours.

---

---

### Case 35 — A person who cannot boot is retried for ever, says so, and comes back on their own

Fix: this change. Provoked live on the operator's own box:
`ivo`, `sasha`, `eli` and `rune` crash-looped through a 90-second chiefd outage
at 12:26 UTC, hit the five-failure limit at 12:34, and were **still down at
14:05** — an hour and a half after the fault causing it had cleared. Their rows
read `starting`, their store rows read `desired_active=1`, and the operator's
clicks did nothing. The give-up had sealed its own exit: a held person is
dropped from placement, so no pane is ever minted for them, so the "a live pane
releases the hold" escape could never fire.

**The ruling, in the owner's words:** *"We need to never give up. Why is there a
crash loop of five? It shouldn't be like that. If something needs to start it
should just start. If it's crash looping, just do a backoff on it with a maximum
of let's say 10 seconds ... Always keep retrying ... and then show some kind of
indication on the screen that it's crashing and this is retry number blah blah
blah, so we can know how many retries happened."*

**What the operator sees when this regresses.** A person stops being retried.
Any of: their retry number stops climbing; the rail row goes back to `starting`
and stays; the round line stops carrying their crash sentence; or the company
does not recover after the fault is removed.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

```bash
export PATH=$PATH:/opt/zipbox/harnesses/defaults/bin:/opt/zipbox/runtime/usr/bin
set -a; . /run/zipbox/placeholders.env; set +a
export DIR=/root/companies/crashloop-labs
rm -rf $DIR && mkdir -p $DIR && cd $DIR      # clean slate, our own directory
chief                                         # Founder creates the company
```

**Precondition, staged the honest way.** Use §4.8's recipe exactly: have the
person write themselves a Pi extension that throws at load. Nothing is faked —
no tmux tag is edited, no store row is written, no binary is corrupted. If the
extension does not make the pane die within a pass, the precondition is NOT met;
report NOT RUN rather than editing anything by hand.

**Criterion 1 — it never stops.** Watch the actuator pane for at least two
minutes after the fifth consecutive failure, which is where the old design gave
up:

```bash
tmux -L $SOCK capture-pane -p -S -400 -t "$ACT" | grep 'has failed to stay up'
```

**PASS:** the count keeps climbing past 5 — 6, 7, 8, … — and a new line appears
about every ten seconds. **FAIL:** the count stops, or the lines stop, or any
line contains the word `STOPPED`.

**Criterion 2 — the numbers the operator asked for are on the screen.** The
round line must carry, for that person: the retry count, how long it has been
going on, when the next attempt is, and a sentence about the error.

```
crashloop-labs: 'dana' has failed to stay up 9 times in a row over 1m 47s;
retrying in 10s and for as long as chiefd wants them up. Last error: ...
```

**PASS:** all four present. A line with a count and no elapsed time is a FAIL —
the elapsed time is how the operator knows whether this started ten seconds ago
or an hour ago, and it is the half the owner asked for by name.

**Criterion 3 — the rail says `crashing`, not `starting`.** In the browser, the
person's row shows the amber filled ring `◉` and the word `crashing`. Clicking
them puts the crash sentence on the focus body — NOT `<name> is starting…`.
Screenshot both.

**Criterion 4 — the backoff is bounded.** The interval between attempts grows
and then settles. Time the gap between consecutive spawn attempts for that
person:

```bash
grep 'actuator.person.crash-looping' /root/.chief/log/chief.jsonl \
  | tail -20 | jq -r '.at + " " + (.detail.failures|tostring) + " " + (.detail.retry_in_ms|tostring)'
```

**PASS:** `retry_in_ms` never exceeds 10000. **FAIL:** any value above it, or a
gap between attempts that keeps growing.

**Criterion 5 — THE HALF THAT MATTERS: it self-heals with no operator action.**
Remove the fault and then **touch nothing else**. No click, no wake, no restart,
no `chief stop`.

```bash
mv $DIR/.chief/agent/dana/.pi/extensions/<name>.ts /tmp/   # the fault, removed
# and now do nothing at all for 60 seconds
```

**PASS:** within ten seconds the person's pane comes up, their row goes
`working`, and their crash lines stop. **FAIL:** anything at all is needed from
the operator. This is the exact thing the owner's box could not do, and it is
the criterion that fails if the give-up ever comes back in another shape —
including as a timer, a watchdog, or a stale-starter sweep.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm crashloop-labs 2>/dev/null; rm -rf $DIR
```

### Case 38 — The rail nests a sub-department under its parent, matching the store

Fix: this change. The Taperoom Inc owner photographed a flat rail — Trading
Strategy and its sub-departments Commodities/Securities/Crypto Strategy drawn as
siblings — while the store correctly nested all three under Trading Strategy.
The store was right (`departments.parent_id`, preorder `ordinal`) and the
sidebar already derived the correct `DepartmentRow.depth`; the renderer and the
disclosure hit-test simply ignored it, so every department drew at one fixed
column. This case builds the real shape the owner had — a department whose
parent is another department, not the root — and shows the rail draws it nested.

**What the operator sees when this regresses.** A sub-department appears at the
same indent as a top-level department, so the rail no longer matches the roster:
Commodities Strategy sits beside Trading Strategy instead of under it.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

```bash
export PATH=$PATH:/opt/zipbox/harnesses/defaults/bin:/opt/zipbox/runtime/usr/bin
set -a; . /run/zipbox/placeholders.env; set +a
export DIR=/root/companies/nesting-labs
rm -rf $DIR && mkdir -p $DIR && cd $DIR      # clean slate, our own directory
chief                                         # Founder creates the company
```

**Precondition — build a genuine two-level tree, nothing faked.** In the CEO
pane, instruct the CEO in plain language to create the structure, so the store
writes the parent links itself (no tmux tag edited, no store row hand-written):

> "Create a department called Trading Strategy, headed by Sage. Then, INSIDE
> Trading Strategy, create a sub-department called Commodities Strategy headed by
> Ore — Commodities Strategy's parent must be Trading Strategy, not the
> executive root."

**Criterion 1 — the store genuinely nests it (ground truth).** The rail can only
be right if the store is; confirm the parent link before reading the glass.

```bash
# sqlite3 is not always present; this reads the durable fact the rail derives.
python3 - <<'PY'
import sqlite3, glob
db = glob.glob('/root/companies/nesting-labs/.chief/db/chief.db')[0]
c = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
for row in c.execute("SELECT id, name, parent_id, ordinal FROM departments ORDER BY ordinal"):
    print(row)
PY
```

**PASS:** `commodities-strategy` (or whatever id the CEO minted) has
`parent_id = trading-strategy`, and `trading-strategy` has `parent_id =
executive`; the `ordinal` column is a preorder walk (Trading appears before its
child Commodities). **FAIL:** the sub-department's `parent_id` is the executive
root — then the store is wrong and this is the transfer bug, NOT this render fix;
report it to that lane and stop.

**Criterion 2 — the rail draws it nested, matching the roster.** In the box's
own Chromium, open the company and expand Trading Strategy. Screenshot the rail.

**PASS:** Commodities Strategy is drawn one indentation step to the RIGHT of
Trading Strategy, directly below it, and its head Ore sits indented under
Commodities Strategy. Trading Strategy is itself one step right of the executive
root. The `+`/`−` disclosure control sits beside each label at its own indented
column, not stranded in a shared left gutter. **FAIL:** Commodities Strategy is
at the same column as Trading Strategy (flat siblings), or its disclosure does
not track its label.

**Criterion 3 — order and counts are unchanged.** The rail order is the store's
preorder (`ordinal`), and each department's `live/total` count matches its
members. **PASS:** no department is reordered and no count is off by the head.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm nesting-labs 2>/dev/null; rm -rf $DIR
```

---

### Case 39 — A transfer that dissolves the mover's single-person department succeeds

Fix: `fix/transfer-dissolve-fk`. Found from a real box (company `4cc439341aa9`), where `org_transfer(person → dept, vacates: dissolve)`
failed EVERY attempt with `store failure: org-manifest-rows:
SqliteFailure(Error { code: ConstraintViolation, extended_code: 787 },
Some("FOREIGN KEY constraint failed"))`. `transfer_person` vacated the headship
FIRST — and on the `Dissolve` answer that DELETEd the emptied department — while
the mover's `people.department_id` still referenced it, so under production's
`PRAGMA foreign_keys=ON` the DELETE broke the `people → departments` FK. The
store-level `org_ops` unit tests run with foreign keys OFF, so the bug was
invisible until it reached a real daemon.

**What the operator sees when this regresses.** A person who is the only member
of the department they head cannot be flattened into a worker of another
department. The move refuses with a system fault (a 500, not a policy 422), and
the person is stuck as the head of their own one-person sub-department. A whole
company can accrete several such stuck heads because no transfer-with-dissolve
ever completes.

**Preconditions.** A fresh company. Create a one-person department: a head with
no other members and no child departments. This is the exact shape the fix is
about — the mover is the department's last member.

**Gesture.** Transfer that head into another existing department AND dissolve
the department they leave, in one gesture — through the CEO: *"move <person>
into <other department> as a worker under its head, and dissolve <their
department>."* The daemon vacates with `vacates: dissolve`.

**Expected.** The transfer APPLIES. The person now homes in the destination
department as a `worker`. Their old single-person department is GONE from the
roster — not headless, not empty, removed. No health incident is raised.

**Verify — the roster is the authority.**

```bash
sqlite3 -readonly $DIR/.chief/db/chief.db "
  select id, name, kind, department_id from people   where slug='$SLUG' and id='<person>';
  select id from departments where slug='$SLUG' and id='<their old department>';
"
grep -c '787\|FOREIGN KEY constraint failed' $DIR/.chief/log/chiefd.jsonl
```

**PASS:** the person's `department_id` is the destination and `kind` is
`worker`; the old department query returns NO row; and the log grep finds no new
787 / FK failure for the gesture. A `TransferOutcome::Applied` (200), never a
500.

**Failure signature.** A 500 with `org-manifest-rows: SqliteFailure(...
extended_code: 787 ..., "FOREIGN KEY constraint failed")` in `chiefd.jsonl`, a
`health incident RAISED` for `tool:org_transfer`, and the person still heading
their untouched single-person department.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

---

### Case 40 — A whole sub-department tree flattens into its parent, and every head becomes a worker

Fix: `fix/transfer-dissolve-fk` (#1171), measured end to end on a live box at
2026-08-19T21:56Z (broken) and 22:01Z (fixed). Case 39 pins ONE
transfer-with-dissolve; this case pins the gesture the operator actually ran —
**collapse a whole subtree** — because that is the shape that reached a real
company (`taperoom-inc` on a live box) and stalled it. Two of the three
units were NOT one-person units until the member move emptied them, so the
sequence exercises the FK-ordering path three times over three different
starting shapes.

**What the operator sees when this regresses.** They ask, in one sentence, for
several small departments to be folded back into their parent — "convert them
all to workers and put them all in Trading Strategy". The workers move. Every
HEAD refuses with a system fault (a 500, not a policy 422), and each stays
stranded over an empty one-person department. The CEO then starts hunting for
another route and reaches for `org_remove_department`, which FIRES people — so
the failure is not merely a stall, it pushes the agent toward a destructive
workaround. A company accretes one stuck head per attempt.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

```bash
export PATH=/root/.chief/bin:$PATH
set -a; . /run/zipbox/placeholders.env; set +a
export NODE_EXTRA_CA_CERTS=/run/zipbox/ca-bundle.crt
export DIR=/root/companies/fk-labs
rm -rf $DIR && mkdir -p $DIR && cd $DIR
tmux new -s host            # the tab is not tmux-backed on every box (§3.3)
chief                       # Founder creates the company
```

**Precondition — build the tree through the CEO, in plain language.** One
message, so the store writes every parent link itself:

> "Build this structure and leave everybody asleep. Create a department Trading
> Strategy headed by Sage. Inside Trading Strategy create three sub-departments:
> Commodities Strategy headed by Ore with no other members; Securities Strategy
> headed by Sam with one worker Tess; Crypto Strategy headed by Niko with one
> worker Kai."

Confirm the tree in the store before the gesture (§4.1's python form). Three
departments must have `parent_id = trading-strategy`.

**Gesture.** One message to the CEO:

> "Now flatten the three sub-departments. Convert Ore, Sam, Niko, Tess and Kai
> all into plain workers of Trading Strategy, and dissolve Commodities Strategy,
> Securities Strategy and Crypto Strategy. Keep everybody asleep. Report exactly
> what each tool call answered, including any error text."

The CEO moves Tess and Kai with `org_move_department_members`, then transfers
each now-sole head with `org_transfer(vacates: dissolve)`.

**Expected.** All five people home in `trading-strategy` with `kind = worker`.
The three sub-departments are GONE from the roster. Trading Strategy still has
its own head (Sage). No health incident, and NOBODY is fired.

**Verify — the roster is the authority.**

```bash
python3 - <<PY
import sqlite3
db = sqlite3.connect("file:$DIR/.chief/db/chief.db?mode=ro", uri=True)
for r in db.execute("SELECT id, parent_id, head_person_id FROM departments ORDER BY ordinal"): print(r)
for r in db.execute("SELECT id, kind, department_id, employment_state FROM people ORDER BY ordinal"): print(r)
PY
grep -c 'FOREIGN KEY constraint failed' $DIR/.chief/log/chiefd.jsonl
```

**PASS:** exactly two departments remain (`executive` and `trading-strategy`);
the five ids all read `worker` / `trading-strategy`; the grep counts **0** new
787 lines for the gesture; and the rail draws Trading Strategy with no children.
Measured on the fix: `Teammate moved` three times, `Trading Strategy 3/6` on the
rail.

**Failure signature.** `Moving teammate failed (system fault) (ref …) · chiefd
unavailable (http-error) at http://127.0.0.1:<port>/v1/org/person/transfer:
store failure: org-manifest-rows: SqliteFailure(Error { code:
ConstraintViolation, extended_code: 787 }, Some("FOREIGN KEY constraint
failed"))` in the pane, a `health incident RAISED` for `tool:org_transfer` with
fingerprint `07d7b05bf4c64a0fa6849160` in `chiefd.jsonl`, and the heads still
over their own empty units.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm fk-labs 2>/dev/null; rm -rf $DIR
```

---

### Case 41 — Click Wake Up on a sleeping person and they come up, and STAY up

Fix: `fix/wake-fence-swept`. Found from the operator's own company
(`taperoom-inc` on a live box, 2026-08-19): clicking a sleeping person
and pressing **Wake Up** did nothing, four clicks in a row. The daemon accepted
every click — `POST /v1/org/person/wake` answered 200 and logged
`org.person.wake.applied person=dev` — and then deleted the grant it had just
made. The durable audit trail is the evidence: `org_events` carries
`launch-intent dev upsert` at 23:24:21.570 and `launch-intent dev delete` at
23:24:22.132, a grant that lived **562 milliseconds**, with no withdrawal note
naming it. That person's Pi kept starting and reported
`interactive-loop-ready` at 23:25:02 — **forty seconds** after the click — into
a company that had stopped wanting them, so the pane was reaped on arrival.

**What the operator sees when this regresses.** They click a sleeping person,
the card opens, they press Wake Up, and the rail goes back to a red dot. No
error, no refusal, nothing on the glass. Clicking again repeats it exactly.

**Why a small company will NOT show it.** The bug is a race with Pi's own boot
time. On an idle two-person company the pane arrives inside one converge pass
and the grant survives; the failure needs a company whose panes take longer
than a pass to start — a loaded box, a big roster, or a person whose agent home
is cold. Run this case on a company with at least a dozen people, or after
waking several people at once.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

**Precondition.** A company with people who are asleep (red dots). Leave them
asleep — do not send them work, because mail is a second, independent demand
and it would hide the defect this case is about.

**Gesture.** In the rail, click one sleeping person. Their card opens with a
**Wake Up** button. Press it. Record the UTC instant (rule 0.7).

**Expected.** A pane appears for that person and STAYS. Their rail dot goes
green and stays green. The wake is honoured for as long as it takes their Pi to
boot, which on a busy box is tens of seconds.

**Verify — the store and the audit trail, not the glass alone.**

```bash
# The grant must still be there a minute after the click.
python3 - <<PY
import sqlite3
c = sqlite3.connect("file:$DIR/.chief/db/chief.db?mode=ro", uri=True)
print("fenced:", sorted(r[0] for r in c.execute("SELECT person_id FROM launch_intent")))
print("touches:", list(c.execute(
    "SELECT at, op FROM org_events WHERE entity='launch-intent' AND entity_id=? "
    "ORDER BY seq DESC LIMIT 6", ("<person>",))))
PY
grep -c 'interactive-loop-ready' $DIR/.chief/logs/pi-startup.jsonl
```

**PASS:** the person is in `launch_intent` a full minute after the click; the
newest `org_events` touch for them is the `upsert`, with **no `delete`
following it**; `pi-startup.jsonl` shows their `interactive-loop-ready`; and
the pane is still there afterwards.

**FAIL — the exact signature.** An `upsert` followed within a second or two by
a `delete` for the same person, `reconcile.people.withheld` naming them
`[MaintenanceBackpressure]` on the next pass and `[nothing-demanded-them]`
after that, and — the tell that separates this from an ordinary settle — an
`interactive-loop-ready` line for them in `pi-startup.jsonl` timestamped AFTER
the delete. That is a pane that came up into a company which had already
withdrawn the request for it.

**Also check the words.** Every withdrawal now names its own reason:
`launch intent withdrawn (settled|not-operational|no-demand): <people>`. A
withdrawal with no note at all, or one that says `settled` about somebody whose
agent never reported, is this defect wearing the wrong label.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

---

### Case 42 — Every person a fresh company hires can actually start

Fix: `fix/identity-key-mint-race`. Found on a real company
(a live box, `4cc439341aa9`, 2026-08-20T00:23Z) where SIX of
twenty-one people never started. `ensure_identity_key` checked `path.exists()`
and then wrote through `rename(2)`, which REPLACES. Four provisioning passes
ran concurrently inside one daemon (`pid 19777`; four
`agent-auth: person identity enrolment pass complete` lines stamped
`00:23:11.507Z`), so two of them minted a key for the same person: one enrolled
its key at `.503`, the other published a different key over the file, and the
warning arrived at `.504`. `identities` and the disk then disagreed for ever,
because rotation is deliberate and nothing re-points the trust table.

**What the operator sees when this regresses.** A brand-new company finishes
hiring, the rail draws everybody, and a handful of people simply never come up.
Each carries a `Cannot start` card reading *"a different identity key is
already enrolled for this person; rotation is explicit and has not been
performed, so they cannot authenticate to chiefd and would exit seconds after
starting"*. It looks like a per-person fault and it is not: which people are
hit is decided by timing, so the same gesture hits a different set each run,
and no retry ever clears it.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

```bash
export PATH=/root/.chief/bin:$PATH
set -a; . /run/zipbox/placeholders.env; set +a
export NODE_EXTRA_CA_CERTS=/run/zipbox/ca-bundle.crt
export DIR=/root/companies/anchor-labs
rm -rf $DIR && mkdir -p $DIR && cd $DIR
tmux new -s host            # the tab is not tmux-backed on every box (§3.3)
chief                       # Founder creates the company
```

**Gesture — hire BROADLY in one instruction, so the passes overlap.** The race
needs several provisioning passes in flight at once; one department at a time
will not reproduce it. One message to the CEO:

> "Build four departments and staff each one: Portfolio Management headed by
> Ivo with three workers, Execution Desk headed by Ana with four workers,
> Strategy Research headed by Bo with four workers, and Market Intelligence
> headed by Cy with four workers. Then start everybody."

**Expected.** EVERY person starts. No `Cannot start` card anywhere on the rail.

**Verify — compare the trust table against the disk, person by person.** This
is the authority; the rail only shows you where to look.

```bash
python3 - <<'PY2'
import sqlite3, subprocess, hashlib, base64, os, glob
DIR = os.environ["DIR"]
db = sqlite3.connect(f"file:{DIR}/.chief/db/chief.db?mode=ro", uri=True)
rows = dict(db.execute("SELECT identity_id, fingerprint FROM identities WHERE kind='person'"))
bad = []
for path in glob.glob(f"{DIR}/.chief/agent/*/chiefd-identity.key.pem"):
    person = os.path.basename(os.path.dirname(path))
    der = subprocess.run(["openssl","pkey","-in",path,"-pubout","-outform","DER"],
                         capture_output=True).stdout
    fp = base64.urlsafe_b64encode(hashlib.sha256(der).digest()).decode().rstrip("=")
    if rows.get(person) != fp:
        bad.append((person, rows.get(person), fp))
print("people:", len(rows), "mismatched:", len(bad))
for row in bad: print(row)
PY2
grep -c 'a different key is already enrolled' $DIR/.chief/log/chiefd.jsonl
```

**PASS:** `mismatched: 0`, every hired person present in `identities`, the
`grep -c` reads **0**, and every pane on the rail is running. The comparison
must be run per person — a company where most people came up looks healthy, and
this defect never hits everybody.

**Failure signature.** One or more people withheld with *"a different identity
key is already enrolled for this person; rotation is explicit and has not been
performed … restore the key that matches it
(`<DIR>/.chief/agent/<person>/chiefd-identity.key.pem`)"*; the python probe
reporting a non-zero `mismatched` count; and in `chiefd.jsonl`, for each hit
person, one `agent-auth: person identity enrolled from its key` immediately
followed — within a few milliseconds, same pid — by repeated
`agent-auth: a different key is already enrolled for this person; rotation is
explicit and was not performed`. On the pre-fix box that warning appeared 182
times over 6 people.

**The neighbouring case this must NOT be confused with.** A key file that
outlives its company and is found by a NEW company in the same directory is
ADOPTED, not refused: the new company's `identities` table is empty, so the
surviving key becomes its anchor. To check that half, `chief rm` the company,
leave `.chief/agent/` in place, create a company in the same directory and
confirm everybody starts. A refusal there is a different defect from this one.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm anchor-labs 2>/dev/null; rm -rf $DIR
```

---

### Case 43 — A person the operator wakes stays wanted, and the wake invents nothing

Fix: `fix/hired-person-never-starts`. Found on a live box, 2026-08-20:
`engineering-kimi3` was hired into Engineering at 17:06:41 and never came up.
The daemon enrolled his identity, granted a launch intent at 17:06:54, called
him `MaintenanceBackpressure` on the next pass and `nothing-demanded-them` on
the one after, and twenty-six passes later he had still never had a pane. His
row read `agent_active_at = 17:08:50` with NO quiet stamp, no launch-intent row
and `last_desired_active = 0` — an agent report from a pane that never existed.

**The mechanism, because it is easy to misread.** The row half of a wake
(`release_idle_park`) used to write `agent_active_at = <the wake instant>` to
buy the person a liveness window while their pane started. Case 41's rule reads
any agent stamp as "they answered", and a grant stays demand only while the
person has said nothing — so the wake's own stamp made the very next pass
conclude the agent had spoken, drop the demand, and sweep the grant the wake had
just made. Every click did this. The person the operator asked for was the one
person the system talked itself out of starting.

**What the operator sees when this regresses.** A newly hired person never
appears. Clicking Wake Up on them does nothing, repeatedly. The rail shows a red
dot, no refusal, no error.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end (rule 0.2).

**Precondition.** A company with the CEO up. Somebody who has run once and gone
quiet — that is the state where the old code stamped the wake instant, so it is
the state that reproduces it. Hiring somebody fresh reaches the same rule by the
other door and is worth running too.

**Gesture.** Click the sleeping person in the rail, press **Wake Up**, and
record the UTC instant.

**Expected.** Their pane appears and stays. The launch-intent row is still there
a minute later.

**Verify — the store, not the glass.**

```bash
python3 - <<PY
import sqlite3
c = sqlite3.connect("file:$DIR/.chief/db/chief.db?mode=ro", uri=True)
print("fenced:", sorted(r[0] for r in c.execute("SELECT person_id FROM launch_intent")))
print("row:", list(c.execute(
    "SELECT last_desired_active, agent_quiet_at, idle_since, agent_active_at "
    "FROM person_activity WHERE person_id = ?", ("<person>",))))
PY
```

**PASS:** immediately after the wake the row reads `agent_quiet_at = None`,
`idle_since = None` and `agent_active_at = None` — *no report yet*, which is the
honest state for somebody about to start — and the person stays in
`launch_intent` until their agent genuinely reports.

**FAIL — the exact signature.** `agent_active_at` equal to the instant of the
click, with no pane for that person; then one pass of
`[MaintenanceBackpressure]`, then `[nothing-demanded-them]` for ever, and an
empty `launch_intent` for them.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

---

### Case 44 — A woken person stays up for the full two minutes with NO message sent

Fix: `fix/wake-lease-floor`. Operator ruling, 2026-08-20: *"If I tell chief to
message it, it'll come back up and do the 2min settling. We need it to always do
that when woken. Message or not. If woken, it needs to wait the 2 mins."*

Found on the operator's own company (`taperoom-inc` on a live box,
2026-08-20). `research-promoter` was woken at 20:34:00.543Z — `launch-intent
research-promoter upsert`, actor `service` — and her grant was deleted at
20:34:02.708Z, **2.165 seconds later**, with actor `''` and no withdrawal note
anywhere in the log. The pass that deleted it printed `launching: …,
research-promoter, …` in the same second. Nothing was ever sent to her.

This is the sibling of Cases 41 and 43 and it is a DIFFERENT defect. Case 41 is a
withdrawal that named itself wrongly and Case 43 is a wake that invented an agent
report; this one is a withdrawal that said nothing at all, and it came from the mail GRANT rather than from any settle: the grant
republished the whole launch-intent document from the daemon's in-memory copy,
which had never seen the row the wake wrote, and the row-mirroring publish
deleted every person the document did not name. Of 597 launch-intent deletes on
that box that day, **310 had no log line at all**.

**What the operator sees when this regresses.** They click Wake Up on a sleeping
person in a BUSY company. The dot goes green for a few seconds and then red
again. Nothing on the glass says why, and the log names somebody else's mail.

**Why a quiet company will NOT show it.** The trigger is mail or session
maintenance for ANYBODY ELSE in the same converge pass — that is what makes the
pass republish the fence at all. Run this on a company where other people are
messaging each other, or send a message between two OTHER people immediately
after the click.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

**Precondition.** A company of at least four people, one of them asleep (red
dot) for longer than the settle window, and at least one other pair actively
messaging. Record the sleeping person's id as `<person>`.

**Gesture.** Click the sleeping person in the rail, press **Wake Up**, and record
the UTC instant (rule 0.7). **Send them nothing.** Then, within ten seconds, send
a message between two OTHER people so the next converge pass carries mail demand.

**Expected.** The woken person comes up and stays up for at least two minutes
from the click, with no message of their own. Their launch-intent row survives
the whole window. After the window, with still nothing to do, they settle
normally and the withdrawal names itself.

**Verify — the store, not the glass.** Immediately after the click, and again 60s
and 115s later; the row must be present every time.

```bash
python3 - <<'PROBE'
import sqlite3
c = sqlite3.connect("file:$DIR/.chief/db/chief.db?mode=ro", uri=True)
print("fenced:", sorted(r[0] for r in c.execute("SELECT person_id FROM launch_intent")))
print("wake:", list(c.execute(
    "SELECT operator_wake_at, last_desired_active FROM person_activity WHERE person_id=?",
    ("<person>",))))
print("touches:", list(c.execute(
    "SELECT at, op, actor FROM org_events WHERE entity='launch-intent' AND entity_id=? "
    "ORDER BY seq DESC LIMIT 8", ("<person>",))))
PROBE
```

**PASS:** `person_activity.operator_wake_at` holds the instant of the click;
`<person>` is in `launch_intent` at every reading up to two minutes after it; the
newest `org_events` touch for them across that window is the `upsert`, with **no
`delete` following it**; and after the window a `delete` appears together with a
`launch intent withdrawn (settled|no-demand): <person>` line in `chiefd.jsonl`.

**FAIL — the exact signature.** In `org_events`, a `launch-intent` `upsert` for
`<person>` with actor `service`, followed within a few seconds by a
`launch-intent` `delete` for the same person **with actor `''` (the empty
string)**, riding in a batch of `person-activity` upserts for the whole roster
that share the same `at`. No `launch intent withdrawn (…)` line names them
anywhere in that window. `reconcile.people.withheld` reports them
`[MaintenanceBackpressure]` on the next pass and `[nothing-demanded-them]`
afterwards, for ever. The empty actor is the tell: it means a whole-document
republish, not anybody's decision about this person.

**Also check the words.** Two lines exist for this now and both are greppable by
person: `launch-intent.wake-lease-held` says a republish RETAINED somebody inside
their lease, and `launch-intent.withdrawn` (with `reason=document-republish`,
`fence-cleared`, or the verb that dropped the row) says one did not. Both are
INFO, deliberately — the converge shrink half commits through the same path, so
these fire on every ordinary settle and a healthy company must not log faults.
A fence delete with neither line is a new silent path and is itself the defect.

**Do not accept a green from a company with no mail traffic.** Without a
concurrent grant the fence is never republished and this case is vacuous.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

### Case 45 — Everybody who stops working actually goes to sleep

Fix: `fix/mail-demand-one-table`. Operator, 2026-08-20: *"there's a few machines
that are just green. They're up. They're connected and they're not going to
sleep. Like after the two minutes settled."*

Found on the operator's own company (`taperoom-inc` on a live box,
2026-08-20 21:38Z). Thirteen people were `last_desired_active = 1` with
`agent_quiet_at` twenty minutes old and `idle_since` NULL, against **zero**
pending mailbox rows in SQL and no open maintenance and no active transition.

This is NOT a settle-path defect and it is NOT Cases 41, 43 or 44 — those are
about a person coming DOWN when they should stay up. This is the opposite: they
could never come down, because the demand holding them up could not be cleared.
`mailbox::enqueue` writes a `pending` row into chiefd's in-memory ledger for
chiefd's own mail (a fired reminder, a health incident); the pane drains it
through `/v1/org/mailbox/delta`, which writes the table and never touches that
ledger; nothing else ever moves the in-memory row. Unioned into demand it is a
`Requested` reason nobody can clear, so `idle_since` is recomputed as NULL every
pass and the quiet lease can never expire.

**What the operator sees when this regresses.** A company where nobody is doing
anything and half the dots stay green for hours. Clicking into a pane shows an
agent that finished long ago. Only restarting the daemon frees them.

**Why a fresh company will NOT show it at first.** The stale rows accumulate
from the moment the daemon starts. The company must run long enough for chiefd
to deliver at least one reminder or health incident, and the recipient must then
drain it. Give it one reminder cycle.

**START FROM NOTHING.** This case creates its own company and deletes it at the
end. Do not run it against a company you did not create (rule 0.2).

**Precondition.** A company of at least three people. Set a one-minute recurring
reminder on one of them (`<person>`), let it fire, and let them read it.

**Gesture.** Do nothing. Send nobody anything. Wait out one full settle window
plus a margin — three minutes from the moment `<person>`'s agent last reported
quiet.

**Expected.** `<person>` settles like anybody else: an idle park is admitted,
their launch intent is withdrawn with a reason, and the dot goes red.

**Verify — the store, not the glass.**

```bash
python3 - <<'PROBE'
import sqlite3
c = sqlite3.connect("file:$DIR/.chief/db/chief.db?mode=ro", uri=True)
print("pending mail:", list(c.execute(
    "SELECT person, COUNT(*) FROM mailbox WHERE state='pending' GROUP BY person")))
print("activity:", list(c.execute(
    "SELECT person_id, last_desired_active, agent_quiet_at, idle_since "
    "FROM person_activity WHERE last_desired_active=1")))
PROBE
```

**PASS:** every person with a non-NULL `agent_quiet_at` older than the lease and
no pending row in `mailbox` has a non-NULL `idle_since`, and within a few passes
is no longer `last_desired_active`.

**FAIL — the exact signature.** `idle_since` NULL on somebody who is
`last_desired_active = 1` with an `agent_quiet_at` older than two minutes, while
`SELECT COUNT(*) FROM mailbox WHERE state='pending'` returns 0 for them. That
pair cannot both be true of a working product: the only thing that forces
`idle_since` to NULL for a desired-active non-CEO is effective demand, and there
is none in the table. `reconcile.people.withheld` will not mention them at all —
they are in the `authorized` count — and the daemon log will show
`mail wake granted launch intent: <person>` for mail nobody can find.

**The one-line discriminator.** Compare the daemon's start time against the
person's newest mailbox row: if every pinned person received chiefd-originated
mail AFTER the daemon started, and the one person who did not settles fine, the
in-memory ledger is the source and the table is not.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

### Case 46 — A company that already exists still opens after an upgrade

Fix: `fix/schema-additive-columns`. Operator, 2026-08-20: *"i cannot run chief
in [this box]."*

**This case is different from every other one in this file, and the difference
is the point: it CANNOT be run on a company this suite created.** Every other
case builds a fresh company, and a fresh company builds its database from the
current schema — which is exactly the blind spot. The subject here is a database
that was created by an EARLIER build.

Found on the operator's own box, 2026-08-20T23:40:12Z. `chief` printed
`chiefd ... did not become healthy within 15s`, and the daemon log read:

```
ERROR chiefd::run: cannot open the company database company=4cc439341aa9
  error=company journal is unreadable: activity rows unreadable:
  no such column: operator_wake_at in SELECT ..., operator_wake_at
  FROM person_activity WHERE slug = ?1
```

`CREATE TABLE IF NOT EXISTS` adds no columns to a table that already exists, so
a column declared by a new build never reaches an old database while the readers
select it by name regardless.

**Precondition.** A company created by the PREVIOUS release and left on disk.
Keep one deliberately: create a company on the build you are about to replace,
`chief stop` it, and record its directory as `$OLD`.

**Gesture.** Install the new build. Then `cd $OLD && chief`.

**Expected.** It starts. The rail comes up with the company's real roster.

**Verify — the store, not the glass.**

```bash
sqlite3 $OLD/.chief/db/chief.db "PRAGMA table_info(person_activity);" | cut -d'|' -f2
```

**PASS:** the daemon log's newest `daemon.boot` line is followed by a
`supervision cycle committed`, and the column list above contains every column
the build's readers select.

**FAIL — the exact signature.** `cannot open the company database ... no such
column: <name>` in `.chief/run/daemon.log`, repeating once per launch attempt,
while a company you create fresh on the same build works perfectly. That pair —
old company dead, new company fine — is the signature of a schema addition with
no reconcile, and it is the whole of this case.

**If it fails, the one-line unblock** (and then file it, because the next
operator will not know it):

```bash
sqlite3 $OLD/.chief/db/chief.db "ALTER TABLE person_activity ADD COLUMN <name> TEXT;"
```

**Do not clean up `$OLD`.** It is the fixture for the next upgrade, and this
case has no value without one.

### Case 47 — Clicking a department shows the department, and disturbs nobody

Fix: `feat/department-overview-card`. Operator, 2026-08-21: *"when I click on the
department show me an overview… something simple, something valuable, some good
metadata"*, and about the surface it replaces: *"it always starts half screen
and then resizes full screen so it's very jarring"*.

**Precondition.** A company with at least one department holding three or more
people, at least one of them awake and at least one asleep.

**Gesture.** Click the department's own row in the rail — the row with the name
and the `n/m` count, not a person under it.

**Expected.** The pane beside the rail draws a card: the department's name and
its place in the tree, a strip and bar of who is up, the models in use, and a
table of every member with their role, their state and their model, with the
head marked. No agent's pane appears, moves, or repaints.

**Verify — the panes, not the glass.**

```bash
tmux -L $SOCKET list-panes -a -F \
  '#{window_id} #{pane_id} #{pane_width}x#{pane_height} #{@organization_person_id}'
```

**PASS:** the window on the glass holds exactly TWO panes — the rail at its
recorded width, and the card — and neither carries an `@organization_person_id`.
Every agent pane in the session sits in some other window, and none of them
changed size across the click.

**FAIL — the exact signature.** The clicked window holds three or more panes, or
any pane in it carries an `@organization_person_id`. That is the retired grid:
the people were moved onto the glass and are now rendering at a fraction of the
width they are read at. The second signature is a size change — record
`#{pane_width}` for one agent before the click and after it; a difference means
a pane was moved between geometries and its whole scrollback was rewrapped.

**Also check what the card SAYS.** Compare it against the store: a person the
rail draws with the settle clock running must read `idle`, and one the launch
gate has declined must read `cannot start` and NOT `asleep` — those are the
product working and a fault you can act on, and one card must never print the
other's word.

**Known limit, and it is not a failure of this case.** A PERSON click still
moves that person's pane out of their department's window into the focus body,
so it still resizes and still reflows. That is the other half of the operator's
report and is tracked separately; this case is about the department row only.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

### Case 48 — "Hire a Chief of Staff" hires a person, and creates no department

Fix: `fix/hire-lands-in-an-existing-department`. Operator, 2026-08-21: *"every
time I tell my chief to hire someone, it always puts them in their own
department… ideally when we say hire it should be in the exact department unless
the user specifically says create a department."*

**Precondition.** A company with a CEO and at least one department that already
has people in it.

**Gesture.** Tell the Chief, in exactly these words and nothing more:

```
hire a chief of staff
```

**Expected.** One new person, in an EXISTING department — the one the Chief
heads — titled "Chief of Staff", with a short human first name. No new
department.

**Verify — the routes, not the glass.** This is the discriminator, because a
department create and a hire both end with a person on the rail:

```bash
grep -o 'path=/v1/org/[a-z/-]*' $DIR/.chief/run/daemon.log | tail -20
```

**PASS:** `/v1/org/person/hire` appears and `/v1/org/department/create` does
not. `SELECT COUNT(*) FROM departments` is unchanged across the gesture.

**FAIL — the exact signature.** `/v1/org/department/create` in the log, and a
new department holding exactly one person whose title is the role you asked for:

```sql
SELECT d.id, COUNT(p.id) FROM departments d
  LEFT JOIN people p ON p.department_id = d.id GROUP BY d.id;
-- chief-of-staff | 1      ← the defect
```

Measured twice in this shape: three one-person departments on one live box
(`growth`, `marketing`, `social-media`) and `chief-of-staff` on another.

**Then check the other direction, because the fix must not break it.** Say:

```
create a growth department
```

`/v1/org/department/create` MUST appear now, with a head. A default that
swallowed an explicit instruction would be the same defect facing the other way.

### Case 49 — The department card is right, and stays right

Fix: `fix/department-card-live`. Operator, 2026-08-21, reading their own rail
and the card beside it at the same instant: the rail drew `Executive 2/5` with
Chief and Sam on green dots, and the card said `0 up · 4 asleep · 1 starting`,
`Chief … starting`, `Sam … asleep`. Both surfaces were correct about the moment
they were drawn, and one of those moments was minutes old.

**Precondition.** A company whose department has at least two people, both
asleep, and a pane at least 160 columns wide. Note the exact model string the
company runs — the longer the better; this case is also about the model column.

**Gesture.** Click the department's row. Read the card. Then click a sleeping
person to wake them, and WITHOUT touching anything else, watch the card.

**Expected.** Within a changefeed wake or two of the person's pane appearing,
that person's row on the card changes from `asleep` to `starting` to `working`,
the strip glyph changes, the bar and the `N up` counts move with it — and the
window does not jump, the rail does not resize, and the operator is not
re-selected anywhere.

**Verify — the panes, not the glass.**

```bash
# The card's pane, its process, and what it records itself as drawing.
tmux -L $SOCKET list-panes -a -F \
  '#{window_id} #{pane_id} #{pane_pid} #{pane_left}x#{pane_width} #{@chief_department_card}'
```

**PASS, and it is three separate readings.**

1. **The card agrees with the rail.** Every person the rail draws on a green dot
   reads `working` or `idle` on the card, and the card's `N up` equals the
   number before the slash on the department's own rail row. `2/5` beside
   `0 up` is the defect, and they are one fact.
2. **It moved.** The card pane keeps the SAME `#{pane_id}`, the same
   `#{pane_left}` and the same `#{pane_width}` across the wake, and its
   `#{pane_pid}` and its `#{@chief_department_card}` stamp both change. Same
   pane, new process, new facts.
3. **Nothing else moved.** Take the whole `list-panes -a` output before the wake
   and after it; every other line is identical. The active window is the one it
   was.

**FAIL — the exact signatures, and they are different faults.**

* The card still says `asleep` a minute after the rail says otherwise, and its
  `#{@chief_department_card}` stamp has not changed: the refresh is not firing.
  Grep the rail log for `sidebar.department.card.repainted`.
* The card's `#{pane_pid}` changes on a company that is NOT moving, or
  `sidebar.department.card.repainted` repeats once a second: the transition
  guard has fallen open. This is the churn loop
  `effects::show_department_overview` documents, and it is the reason this
  effect is allowed on the refresh path at all — it re-lays, re-selects and
  wakes the other rail until the glass is unusable.
* The window is re-selected, or any pane changes width, when the card
  repaints: the repaint is laying out or navigating. It may do neither.

**Check the OTHER card too, and this is the half that was got wrong first.** A
session holds one overview window per department you have clicked, so click a
second department, then go back and change something about the FIRST one. Both
cards must follow. The repair for this case originally refreshed only the
department the rail had SELECTED, so the second card froze at whatever it said
when it was minted — the same defect, inside its own fix. The signature is
precise and easy to check:

```bash
tmux -L $SOCKET list-panes -a -F '#{pane_id} #{@chief_asleep_for} #{pane_pid} #{@chief_department_card}' \
  | grep __overview__
```

Every overview pane in that list must carry a stamp, and the stamp of a card
whose department has moved must change even when the operator is looking
somewhere else entirely. A selection says what somebody is LOOKING at; it has
never said what is true.

**Also check the MODEL column.** On a wide pane, the model must be printed in
full. `openrouter/deepseek/deepseek-…` beside a right-hand half of the pane that
is blank is the defect: the columns are allocated from what the members actually
need, so a model is ellipsised only when the pane genuinely cannot hold it.
Narrow the pane by dragging the rail wider and the ROLE column gives way first;
the model is the last column to lose a cell.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

### Case 50 — A dragged rail stays where the operator left it, after the company has moved on

Fix: `fix/sidebar-drag-sticks`. Operator, 2026-08-21, after upgrading a live box
to the #1196 build and finding it unchanged: *"every time I resize the sidebar it
resizes it back."*

**Why this case exists at all.** #1196 shipped as this fix and was a no-op, and
its test passed the whole time — it asserted the SHAPE of
`@chief_viewport_width_command` rather than performing a drag. So this case is
written to be failable by that build: **it advances the topology epoch before it
drags.** A run that drags a company which has not moved since the hook was
installed proves nothing, because the frozen number and the live one still agree.

**Precondition.** A company open on the glass with an ordinary (non-control-mode)
client attached and a rail drawn — `chief attach` in a real terminal, not a
control-mode host. At least one department, so there is something to click.

```bash
SOCKET=<the company socket under /tmp/tmux-0/>
S=<the org-…_ session name>
tmux -L $SOCKET display-message -p -t $S \
  't=#{@chief_viewport_topology_epoch} m=#{@chief_viewport_manifest_epoch} cols=[#{@chief_sidebar_columns}]'
```

Record all three. `cols` is normally empty on a company nobody has dragged.

**Gesture, in order — the ORDER is the case.**

1. **Move the company on.** Click a department row in the rail. Any window churn
   mints a new topology epoch; a department click mints an overview window.
   Re-read the line above and confirm `t` has changed.
2. **Drag the rail's right-hand border** with the mouse, to a visibly different
   width, and let go. Pick something unmistakable — 40 against the default 26.

**Verify.**

```bash
tmux -L $SOCKET display-message -p -t $S \
  't=#{@chief_viewport_topology_epoch} m=#{@chief_viewport_manifest_epoch} cols=[#{@chief_sidebar_columns}]'
tmux -L $SOCKET list-panes -s -t $S -F '#{window_id} #{pane_id} sb=#{@organization_sidebar} w=#{pane_width}'
```

**PASS — and this is the whole signature.** `@chief_sidebar_columns` is
NON-EMPTY and equals the width you dragged to, **after** the epoch change of step
1; and every pane with `sb=1` reports that same `w=`. Then click another
department and read both lines again: the width holds across the layout that
click provokes.

**FAIL — the exact signature.** `cols=[]`, empty, with the rails back at 26. The
drag was refused. It is refused silently on purpose — the
`MouseDragEnd1Border` binding ends in `|| :` — so the terminal shows you
nothing; the empty option IS the error message. Two more readings tell you which
refusal it was:

```bash
tmux -L $SOCKET show-options -qv -t $S @chief_viewport_width_command
grep 'viewport.callback.failed' $DIR/.chief/log/chief.jsonl | tail -5
```

* A **number** in the width command, between the `$n` session id and the 32-hex
  nonce, is the #1196 defect: an epoch frozen into the option by `set-option -F`.
  The current command has exactly five stored operands and no number among them —
  socket, session, organization, `#{q:session_id}`, nonce — and tmux appends
  `#{pane_width}` as the sixth.
* `sidebar width event no longer belongs to the same company session` in the log,
  on a company you have not restarted, means an identity operand did not match.
  That is a real refusal and a real bug; report it with all three epoch readings.

**If you cannot drive a physical mouse** — a headless QA box usually cannot — say
so in the report and fire the binding's own payload instead. It is the binding
minus its `if-shell` guard on the pane under the pointer, with the released width
standing in for `#{pane_width}`:

```bash
tmux -L $SOCKET run-shell -t $S '#{@chief_viewport_width_command} 40'
```

A report that used this route must say so in the same sentence as the verdict.
The unit half of this case — a real `MouseDragEnd1Border` payload, fired as a
real `run-shell -b` job, against a company whose topology epoch has moved past
hook-install time and whose manifest epoch trails it — is
`attach::tests::real_border_drag_sticks_after_the_company_topology_moves_on`,
under `cargo test -p chief-cli --bins`.

**Clean up after yourself.**

```bash
cd $DIR && chief stop
cd / && chief rm <slug> 2>/dev/null; rm -rf $DIR
```

## 7. Reporting

### 7.1 On success

Report, in one message: the SHA under test, the target hostname, UTC start and
end, the company slug and directory, and a one-line verdict per case. Include the
final round line and the final pane inventory. State the glibc versions of both
boxes and the three binary digests — a green run against an unverified install
proves nothing.

### 7.2 On failure

Stop. Then report **all** of:

| Field | How to get it |
|---|---|
| UTC timestamp of the gesture and of the observation | `date -u +%Y-%m-%dT%H:%M:%SZ` |
| SHA under test | `git rev-parse HEAD` in the build worktree |
| Screenshot paths | the 200 ms / 1 s / 2 s set for that gesture |
| tmux capture paths | `capture-pane -p -S -300` for the actuator, the rail, and the pane in question |
| Wake POST counts | before and after, from §4.3 |
| Pane ids and pids | the before/after inventory diff |
| Tags | every tag on the affected pane, each marked **absent** / **present-and-empty** / **present with value** |
| Log lines | the daemon-log window around the gesture, and any `refus` / `park` / `recall` / `desired=` line in it |

Say plainly which case failed and what the expected result was. **Do not
speculate about the cause in place of the evidence** — a report that names a
suspected commit but omits the pane inventory cannot be acted on; one that omits
the suspicion but carries the inventory can.

If the failure is in Stage A or Stage B, say so explicitly: an installation
failure reported as a product failure sends somebody hunting a bug that is not
there.

---

## 8. Command crib

```bash
# names
SLUG=<slug>; DIR=/root/companies/$SLUG
DAEMON=$(python3 -c 'import json;print(json.load(open("'$DIR'/.chief/run/daemon.json"))["url"])')
SOCK=<socket from /v1/org/runtime-owner/read>
CO=$(tmux -L $SOCK ls -F '#{session_name}' | grep "^org-$SLUG-")
ACT="chiefd-actuator-$CO"

# watch
tmux -L $SOCK capture-pane -p -S -200 -t "$ACT" | tail -30
tail -f $DIR/.chief/run/daemon.log
grep -c 'path=/v1/org/person/wake' $DIR/.chief/run/daemon.log        # requests
grep -c 'event="org.person.wake.applied"' $DIR/.chief/run/daemon.log # applied (never + recalled)

# inventory
tmux -L $SOCK list-panes -a -F '#{session_name} #{window_index} #{pane_id} #{pane_pid} #{pane_width}x#{pane_height} #{pane_current_command}'
tmux -L $SOCK show-options -p -t <pane-id>

# state reads (all read-only)
cd $DIR && chief ls
cd $DIR && chief topology
curl -s -XPOST $DAEMON/v1/org/activity/read       -H 'content-type: application/json' -d '{}'
curl -s -XPOST $DAEMON/v1/org/runtime/read        -H 'content-type: application/json' -d '{}'
curl -s -XPOST $DAEMON/v1/org/runtime-owner/read  -H 'content-type: application/json' -d '{}'
curl -s -XPOST $DAEMON/v1/org/health-monitor/read -H 'content-type: application/json' -d '{}'

# browser
zipbox-desktop status || zipbox-desktop up
playwright-cli -s=desktop attach --cdp=http://127.0.0.1:9222
playwright-cli -s=desktop screenshot --filename=.playwright-cli/<case>-$(date -u +%H%M%SZ).png
playwright-cli -s=desktop detach          # never `close`
```

---

## 9. Leave the box as you found it

**A run that ends without cleanup has not finished.** Reporting is not the last
step; this is. The operator's instruction is one sentence and both halves are
binding: *"you need to always start from scratch … and then when you're done you
need to clean up after yourself."*

**QA / TEST BOXES ONLY**, on exactly the terms §0.5 states. The same warning
applies with the same force, and for a stronger reason here: at this point in
the run the box holds YOUR company, so the destructive commands feel safe — and
that is precisely when somebody runs them against the wrong host.

### 9.1 What to remove

Run **§0.5.1 in full**, then also remove what this run created outside the
company directories:

```bash
ssh root@$TARGET '
  rm -rf /root/companies/<slugs you created, BY NAME>
  rm -rf /tmp/topo-1.txt /tmp/topo-2.txt          # §Case 11 scratch
  rm -rf /root/suite-scratch /root/*-probe.sh     # probe scripts, if you wrote any
'
```

Then run **§0.5.2** and assert the same five zeros. Cleanup you did not verify is
the next run's inherited state, which is the fault §0.5 exists to prevent — so
the last act of this run is the first check of the next one.

### 9.2 PRESERVED EVIDENCE IS THE ONE EXCEPTION

**Cleanup must never silently delete the evidence that explains a failure.** A
crash scene deliberately kept for diagnosis survives — on 2026-08-18 that was
`/root/crash-2226-0818/` and `foundboot-labs.crash-221740Z`.

The rule has two halves and the second is the one that gets forgotten:

1. Do not delete it.
2. **NAME it in the report** — what was kept, where it is, and which case it
   belongs to. Evidence nobody can find is evidence nobody kept. A report that
   says "preserved the crash scene" without a path has thrown it away as surely
   as `rm` would.

Everything you preserve is state the next run will inherit, so §0.5.2's counts
will not read zero for it. That is expected and correct: list the preserved
paths beside the non-zero reading so the next runner can tell deliberate
evidence from leftover mess. An unexplained non-zero is a stop; an explained one
is a handover.

### 9.3 What the report must say

Add one line to §7: **cleanup performed, verified, and what was deliberately
kept.** Three states, and say which:

* `clean` — §0.5.2 reads five zeros.
* `clean, evidence preserved at <paths>` — zeros except the named paths.
* `NOT CLEAN` — with the readings and why. This is a finding, not a footnote:
  the next run will inherit it and will not know.
