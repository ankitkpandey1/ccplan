# ccplan — Working Notes

> **READ THIS FILE FIRST, before starting any implementation step.**
> It is the project's durable memory. Context windows get compacted and summarized; this file does
> not. Every decision, gotcha, learning, and observation lives here so no knowledge is lost between
> sessions or agents. **Append to it as you work** — if you decide something, learn something, or
> get surprised by something, write it down here in the same commit.

This folder (`development/`) is **dev-only scaffolding** (notes, checklist, audit log). It is not
part of the shipped product and may be deleted before/at the open-source release. Keep all
implementation-process docs here.

---

## 0. Orientation (what to read, in order)

1. This file (`development/notes.md`) — decisions & gotchas.
2. `DESIGN.md` (repo root) — the locked product/architecture spec. The source of truth for *what*.
3. `development/implementation_checklist.md` — the staged build plan. The source of truth for *how* and *in what order*.
4. `development/backlog.md` — tasks discovered during implementation (separate from the stable stage plan).
5. `development/audit_log.md` — append your per-stage evidence reports here.

---

## 1. Locked decisions (do not relitigate without updating DESIGN.md)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **Language: Rust, edition 2024**, single binary per OS. | Fast fire target; cross-platform; locked by user. |
| D2 | **Plan/config format: TOML** (crate `toml` 1.x), NOT YAML. | `serde_yaml` is **archived**; `serde_yml` carries **RUSTSEC-2025-0068** + provenance issues; `serde_yaml_ng` is low-maintenance. `cargo-deny` would reject them. TOML is maintained, audited, and the least ambiguous format for agents/humans to edit. |
| D3 | **Date/time: `jiff` 0.2** (`serde` feature). | Best-in-class IANA-zone + DST disambiguation; forces ambiguous-time resolution; bundles tzdb on Windows. Accept pre-1.0 (pin `"0.2"`). |
| D4 | **No background daemon.** Native scheduler on every platform. | Locked by reviewer. systemd user timers / launchd agents / Windows Task Scheduler. |
| D5 | **Schedule-only `rev`.** Hash covers only `id` + timing (`start`, `end`/`duration`, `notify`); excludes `status`, history, `title`, `tags`, `run`. | So lifecycle/content edits never invalidate live triggers (the `end`-trigger bug). See DESIGN §6.3, Inv-15. |
| D6 | **Event-specific fire reconciliation.** `notify` overdue = no-op only; `start` overdue = `missed`; `end` overdue while active = `expired`/`done`. | A late lead-notification must never mark a block missed. DESIGN §7. |
| D7 | **At-most-once via durable ledger** keyed `(date,id,event,rev,scheduled_at)`, atomic check-and-set before notify/run. | Survives scheduler retries / double-invoke / races. Inv-14, §6.4. |
| D8 | **`run:` is argv-only, allowlisted, off by default.** No shell mode ever (NG9). | Plan file is a trust boundary; minimize it. DESIGN §9. |
| D9 | **Immutable history.** `done/skipped/missed/expired` blocks never silently destroyed; `set` retains them even if omitted; only `--override-history` / `clear --purge` touch them. | Inv-7. |
| D10 | **Library + thin-binary split + trait-injected side effects** (`Clock`, `Scheduler`, `Notifier`, fs root). | The only way to reach 100% coverage; lets tests avoid real OS scheduling. |
| D11 | **Dual license `MIT OR Apache-2.0`.** Repo currently has Apache-2.0 only → rename to `LICENSE-APACHE`, add `LICENSE-MIT`, set `license = "MIT OR Apache-2.0"`. | Rust-ecosystem convention. |
| D12 | **100% line coverage gate**, with documented `#[coverage(off)]` exclusions for platform real-IO backends and the `main` shim. | "100%" means 100% of testable logic; platform `#[cfg]` real-IO can't run on one CI OS. |
| D13 | **Branch model:** all work on `dev`; CI runs on every push/PR; merge `dev`→`main` only when production-ready; `release-plz` on `main` maintains a release PR; merging it tags `vX.Y.Z` which triggers the `dist` release build. | User-requested; see checklist Stage 9. |
| D14 | **Engineering mandates (hard):** everything SOTA + idiomatic Rust (edition 2024); **strong type safety — make illegal states unrepresentable** (newtypes, enums-not-strings, parse-don't-validate); **`#![forbid(unsafe_code)]`** — zero unsafe in our code (backends chosen to avoid it, e.g. `schtasks` shell-out over COM — see D16); **comments explain WHY (design decisions/invariants) only, never WHAT** — no code-narration. | User-mandated; enforced in `CONVENTIONS.md` + checklist + clippy pedantic. |
| D15 | **Per-stage rhythm:** every stage = Recon/Research → Implement (TDD) → Self-review & fix → DoD gate → Self-reflect (learnings → notes §6) → capture discovered tasks → audit entry → commit. Discovered tasks go to **`development/backlog.md`** (separate from the stable stage plan), never inline into the checklist. | User-mandated; see checklist HOW TO USE + PER-STAGE RHYTHM. |
| D16 | **No archived/unmaintained dependencies.** Windows scheduling therefore **shells out to `schtasks.exe` (XML task)** instead of `planif` (archived) or `windows`-rs COM (would force `unsafe`). All three backends shell out to the native scheduler CLI — uniform and dependency-light. | User-mandated ("do not use archived dep"); preserves D14's no-unsafe rule. |
| D17 | **Commit convention:** Conventional Commits, but **no `Co-Authored-By` / "Generated with Claude" trailer** of any kind. Clean commit messages only. | User-mandated. |
| D18 | **Performance is a feature.** The binary is a scheduler fire-target invoked many times a day — fast startup, no needless allocation/cloning, no blocking work on the hot path; borrow over clone, stream over buffer where it matters. Don't micro-optimize pure-logic clarity, but never ship gratuitously wasteful code. | User-mandated. |
| D19 | **"Don't Make Me Think" CLI UX** (Steve Krug). The interface must be self-evident: obvious command names, consistent flag patterns, helpful `--help` on every command, error messages that say exactly what's wrong **and** how to fix it, sensible defaults so the common path needs almost no flags, and `doctor` to diagnose setup. Zero cognitive load for the common case. | User-mandated. |
| D20 | **Ship a tested agent skill.** Agents are the primary users, so the project ships `AGENTS.md` + a loadable `skills/ccplan/SKILL.md` (install check → `set --from -` → `apply`, exit codes, JSON contract), and an **agent-onboarding test** (`tests/agent_docs.rs`) that parses the skill frontmatter and runs its documented commands against a temp store so the docs can't drift from the real CLI. README gives non-interactive agent install instructions. | User-mandated. |

---

## 2. Pinned toolchain & dependencies (verified 2026-06-08)

Use these versions as the floor. `cargo add` will pick the latest compatible; pin majors as shown.

```toml
[package]
edition      = "2024"
rust-version = "1.85"            # MSRV; edition 2024 stabilized in 1.85

[dependencies]
clap          = { version = "4", features = ["derive"] }
toml          = "1"
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
jiff          = { version = "0.2", features = ["serde"] }
directories   = "6"
blake3        = "1"
notify-rust   = "4"
fs2           = "0.4"
thiserror     = "2"             # library error enum (CONVENTIONS §4); required, do not omit
anyhow        = "1"             # main/CLI boundary only

[target.'cfg(target_os = "macos")'.dependencies]
plist         = "1"             # maintained; authors the LaunchAgent plist

# Windows: NO extra dependency. We shell out to `schtasks.exe` with an XML task definition
# (see D16) — this avoids the archived `planif` crate AND avoids `windows`-rs COM, which would
# force `unsafe` in our code. All three platforms shell out to the native scheduler CLI.

[build-dependencies]
clap          = { version = "4", features = ["derive"] }
clap_complete = "4"
clap_mangen   = "0.2"

[dev-dependencies]
assert_cmd    = "2"
assert_fs     = "1"
predicates    = "3"
tempfile      = "3"
insta         = { version = "1", features = ["json", "redactions"] }
proptest      = "1"

[lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(coverage,coverage_nightly)'] }
```

Tooling (installed in CI, not deps): `cargo-llvm-cov`, `cargo-deny`, `cargo-dist` (`dist`),
`release-plz`. CI actions: `dtolnay/rust-toolchain`, `Swatinem/rust-cache@v2`,
`taiki-e/install-action`, `codecov/codecov-action@v5`, `release-plz/release-plz-action@v0.5`,
`EmbarkStudios/cargo-deny-action@v2`.

---

## 3. Platform gotchas (hard-won; from research — confirm in practice)

### Linux — systemd `--user` transient timers (shell out to `systemd-run`)
- One-shot at absolute local time: `systemd-run --user --unit="ccplan-<date>-<idhash>-<rev>-<event>"
  --on-calendar="YYYY-MM-DD HH:MM:SS" --timer-property=AccuracySec=1s <abs-path-ccplan> fire …`.
  **The unit name must carry `date` + `idhash` + `rev` + `event`** (full trigger identity) so
  `clear --date` is unambiguous — same for the launchd label and the Task Scheduler name below.
- **Default `AccuracySec` is 60s** → must set `AccuracySec=1s` or alerts drift up to a minute.
- Transient timers auto-clean (`systemd-run` sets `RemainAfterElapse=no`) — no manual cleanup needed.
- **Do NOT set `Persistent=true`** (that's catch-up replay; we forbid it, NG8).
- Use the **absolute path** to the binary — the user manager's PATH is minimal.
- **Notification env:** the user manager has a sanitized env. Pass
  `--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus` (and capture `DISPLAY`/
  `WAYLAND_DISPLAY` at `apply` time and `--setenv=` them) or notifications silently fail.
- Validate the calendar string with `systemd-analyze calendar "<ts>"` and surface errors.
- List: `systemctl --user list-timers 'ccplan-*'`. Cancel: `systemctl --user stop <unit>.timer` (transient → unloads).
- Idempotent apply: `stop` the unit name first (ignore "not loaded"), then `systemd-run`.

### macOS — launchd LaunchAgents (`plist` crate to author + `launchctl` to load)
- `StartCalendarInterval` is **inherently recurring** (no `Year` key) → a one-shot must **self-destruct**.
  The `fire` path must, as its last step, `launchctl bootout gui/<uid>/<label>` and delete its own
  plist. (Look up its own label from an arg/env baked into the plist.)
- Do NOT use `RunAtLoad` (fires at load, not at the scheduled time).
- Plist at `~/Library/LaunchAgents/io.ccplan.<date>.<idhash>.<rev>.<event>.plist`; `Label` = filename stem.
- Load: `launchctl bootstrap gui/$(id -u) <plist>`. Cancel: `launchctl bootout gui/$(id -u)/<label>` + rm.
- Needs an **active GUI (`gui/<uid>`) session**; over pure SSH `bootstrap gui/<uid>` fails.
- Notifications: unsigned bare CLI binaries may be silently dropped; a bundle id helps. `osascript -e
  'display notification …'` is the pragmatic fallback. (notify-rust handles the API; delivery is the caveat.)

### Windows — Task Scheduler (shell out to `schtasks.exe` with an XML task; see D16)
- **No crate.** Do NOT use `planif` (archived) or `windows`-rs COM (would force `unsafe`, violating
  `#![forbid(unsafe_code)]`). Shell out to `schtasks.exe`, consistent with the Linux/macOS backends.
- **Create:** write a task XML to a temp file, then `schtasks /Create /TN "\ccplan\<date>-<idhash>-<rev>-<event>"
  /XML <file> /F` (task name carries the full identity — date + idhash + rev + event). The XML gives full control and **second precision** (unlike `/ST`, which is `HH:mm`
  only). Minimal XML shape:
  - `<TimeTrigger><StartBoundary>2026-06-08T15:30:00</StartBoundary><EndBoundary>…</EndBoundary></TimeTrigger>`
  - `<Settings>`: `<DeleteExpiredTaskAfter>PT0S</DeleteExpiredTaskAfter>` + `<Hidden>true</Hidden>` +
    `<StartWhenAvailable>false</StartWhenAvailable>` (no late catch-up — matches NG8).
  - `<Actions><Exec><Command>` = absolute path to `ccplan.exe`, `<Arguments>` = `fire …`.
  - `<Principal>` running as the interactive user, **only when logged on** (needed for notifications).
- **Namespace:** the `\ccplan\` task-folder prefix in `/TN`. **List:** `schtasks /Query /TN \ccplan\
  /FO LIST` (or `/XML`). **Delete:** `schtasks /Delete /TN "\ccplan\<…>" /F`. Idempotent: `/Create /F`
  overwrites; pre-delete not required.
- **Auto-cleanup:** `EndBoundary` + `DeleteExpiredTaskAfter=PT0S` makes the one-shot task delete itself
  after firing (the Windows analog of systemd's transient auto-clean).
- **Console flash:** the task launches `ccplan.exe`; to avoid a console window, compile the fire path
  as a `#![windows_subsystem = "windows"]` GUI-subsystem shim. (`<Hidden>` hides the task in the UI,
  not the console.)
- **Gotcha:** `schtasks` query output is locale-dependent and brittle to parse — rely on our own
  `triggers.json` for state, and use `schtasks` only for create/delete (clean) and existence checks.

### Notifications — `notify-rust` 4
- Basic title+body works on all three OSes from one API: `Notification::new().summary(t).body(b).show()?`.
- **Action buttons** (Done/Snooze) are **Linux-only** in notify-rust and need a **resident/blocking
  process** to receive the callback (`wait_for_action` blocks). Our fire path is fire-and-exit →
  action buttons are a **later** feature, Linux-first, and must be `#[cfg]`-gated. Don't attempt cross-platform.

---

## 4. Testability notes (critical for the 100% gate)

- **`jiff` has NO clock mocking** (the author declined it; no `JIFF_NOW`). You **must** inject a
  `Clock` trait; real impl calls `jiff::Zoned::now()`, test impl returns a fixed `Zoned`.
- Same pattern for `Scheduler` and `Notifier`: trait + real impl (shell-out / native) + recording fake.
  Logic depends on the trait; tests inject fakes and assert on recorded calls. Real OS scheduling is
  NEVER touched in unit tests.
- **Two entrypoints (the test seam):** `pub fn run(cli, out) -> Result<()>` builds the *real*
  `Context { clock, scheduler, notifier, store }` and delegates to
  `pub fn run_with_context(cli, out, &Context) -> Result<()>`. Fake-backed integration tests call
  `run_with_context` with recording fakes over an `assert_fs::TempDir`; `assert_cmd` (real binary) is
  reserved for parse/`--help`/exit-code and temp-store paths that don't need a fake scheduler.
- `main.rs` stays ~10 lines: parse → `run` → `ExitCode`. The `assert_cmd` e2e tests cover it; it may
  also be `#[coverage(off)]`.
- Coverage exclusions (the ONLY allowed ones): platform `#[cfg(target_os=…)]` real-backend modules,
  the real `SystemClock`/shell-out impls, `unreachable!()`/defensive `panic!`, and `main`. Mark with
  `#[cfg_attr(coverage_nightly, coverage(off))]`. Put `#![cfg_attr(coverage_nightly,
  feature(coverage_attribute))]` at the crate root.
- Coverage runs on **nightly** with the cfg set:
  `RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov …` — without that cfg the
  `#[cfg_attr(coverage_nightly, coverage(off))]` exclusions don't compile/apply. The crate must still
  build/clippy clean on **stable** (where `coverage_nightly` is unset, so those attrs are no-ops).

---

## 5. Open items / things to verify during build

- Confirm the exact `schtasks /Create /XML` task-XML schema (TimeTrigger + EndBoundary +
  DeleteExpiredTaskAfter + Principal) on a real Windows runner.
- Confirm `notify-rust` macOS delivery from a launchd-spawned process (TCC/bundle-id behavior).
- Decide how to integration-test the real schedulers in CI (likely `#[ignore]`d tests run per-OS, or a
  `--features integration` gate). Real timer firing in CI is flaky — keep it minimal and explicit.
- Decide whether `ccplan fire` on macOS does its own bootout, or a tiny `fire-once` wrapper does.
- `directories` v5 vs newer — resolved in Stage 3: use `directories = "6"` so the storage layer stays
  current; `cargo-deny` explicitly allows the transitive OSI-approved `MPL-2.0` license from
  `option-ext`, with no advisory or duplicate-version exceptions.

---

## 6. Running log (append entries here — newest at top)

> Format: `### YYYY-MM-DD — <stage/topic>` then bullets. Record decisions, surprises, dead-ends,
> and anything a future session must know. This is the anti-amnesia log.

### 2026-06-08 — Stage 4 CLI surface + fake-backed apply/fire
- Stage 4 precondition: re-read `notes.md`, `backlog.md`, `implementation_checklist.md`, and DESIGN
  §6/§7/§8 before coding; re-ran the Stage 3/global gate and confirmed the predecessor was green.
- The command layer now uses `ContextRefs` internally: `Context<C,S,N>` remains strongly typed for
  production and fakes, while command dispatch borrows trait objects for `Clock`, `Scheduler`, and
  `Notifier`. This avoids duplicate generic coverage artifacts and keeps command behavior compiled
  through one implementation.
- `run(cli,out)` builds the runtime context from `Store::for_user()` and the unavailable Stage 5
  scheduler/notifier. Tests call `run_with_context` with `FixedClock`, `RecordingScheduler`, and
  `RecordingNotifier`. Binary smoke tests use `CCPLAN_ROOT` to keep real process tests isolated.
- `apply` computes future notify/start/end trigger descriptors from the current plan, diffs them
  against `triggers.json`, calls the injected scheduler for add/remove unless `--dry-run`, and persists
  the trigger ledger only after successful fake scheduler convergence. `clear --yes` uses the same
  reconciler with an empty desired set before archive/purge.
- `fire` is ledger-first and idempotent: missing plan/block, stale rev, and already-fired events no-op;
  active decisions notify, activate, miss, or close through the pure lifecycle table. `run:` is
  deliberately logged as `run-deferred` until Stage 6; no shell execution path exists in Stage 4.
- `status`, `doctor`, and `completions` are non-interactive Stage 4 stubs. Real backend diagnostics and
  generated completions/man pages stay in Stages 5 and 7.
- Coverage gotchas: closure lines inside `ok_or_else` counted as missed until explicit missing-plan and
  missing-block command tests covered them. The real `SystemClock`, `Store::for_user`, and unavailable
  scheduler/notifier methods remain `coverage(off)` as runtime/OS boundaries, not business logic.
- Dependency cleanup: inline `insta` snapshots were replaced with `serde_json::json!` equality checks
  because `insta` pulled an extra dev-only dependency chain and duplicate `cpufeatures`. After pruning
  it, `cargo tree --duplicates` is clean again.

### 2026-06-08 — Stage 3 atomic store + fired ledger + triggers
- Stage 3 precondition: re-read notes/backlog/checklist plus DESIGN §6.2/§6.3/§6.4 and Inv-7/Inv-9/
  Inv-14; re-ran the Stage 2/global gate before implementation.
- Storage is now centralized in `src/store.rs`. `Store::new(&Path)` maps injected test roots to
  `data/ccplan`, `config/ccplan`, and `state/ccplan`; `Store::for_user()` uses `directories::ProjectDirs`
  for the real platform data/config/state dirs.
- B-001 resolved: keep the implementation current with `directories = "6"` rather than downgrading to
  the design's older pinned major. Trying `directories 5.0.1` still pulled `option-ext` and also added
  duplicate older Windows/`thiserror` transitive versions. `directories 6.0.0` keeps the duplicate graph
  clean. `deny.toml` now explicitly allows `MPL-2.0` for the OSI-approved transitive `option-ext` crate.
- Plan writes are locked with `fs2` and use temp-file → `sync_all` → rename. Parent-directory fsync is
  intentionally not attempted with `std` because it is not portable on Windows; the file data is fsynced
  before the atomic rename.
- `set_plan` preserves terminal history by id and also rejects timezone changes when existing terminal
  blocks would be reinterpreted under a new plan timezone. `HistoryPolicy::Override` is the explicit
  escape hatch.
- The fired ledger is durable JSON keyed by `(date, block_id, event, rev, scheduled_at)`. Trigger records
  are durable JSON descriptors with backend id + date/id/event/rev/scheduled_at. `fire_log_path()` is
  exposed under `data/ccplan/log/fire.log` for the later fire stage.
- Coverage gotchas: low-level OS error mapping helpers are `coverage(off)` because write/sync/rename and
  lock API failures are platform-controlled. A small internal store unit test exercises `atomic_write`
  in the library test binary so source-coverage line accounting stays at 100%.
- CI gotcha: Windows reports `fs2::try_lock_exclusive` contention as `PermissionDenied` / raw OS lock
  errors (`ERROR_SHARING_VIOLATION`/`ERROR_LOCK_VIOLATION`), not just `WouldBlock`. `StoreError::Locked`
  now classifies those post-open lock failures as lock contention.

### 2026-06-08 — Stage 2 DST time + pure lifecycle
- Stage 2 precondition: re-read `notes.md`, `backlog.md`, `implementation_checklist.md`, and DESIGN
  §7/§12 before coding; re-ran the Stage 1/global gate and confirmed the branch was clean.
- `jiff::civil::DateTime::to_zoned(TimeZone)` applies the Compatible strategy directly: spring-forward
  gaps move to the next real local time, and fall-back folds choose the earlier occurrence. Stage 2
  documents that choice in `src/time.rs` and tests normal, gap, and fold cases.
- `Clock` is now injectable in `src/time.rs`: `SystemClock` is the only direct `Zoned::now()` caller
  and is excluded from coverage as real time I/O; `FixedClock` is gated behind `test-fakes`.
- Lifecycle decisions are pure: `decide_fire` depends only on `Block`, `Event`, scheduled timestamp,
  current timestamp, and `LifecyclePolicy`. The policy carries both grace and the end behavior because
  DESIGN §7 allows `fire(end)` to close as either `done` or `expired`.
- Grace is interpreted exactly as DESIGN §7 states: `now > target + grace` is overdue; equality at the
  boundary is still on-time. Tests cover `+60s` vs `+61s`.
- `reconcile_overdue` emits deterministic sorted status updates and follows the sleep/off path:
  overdue pending blocks become `missed`; overdue active blocks become `expired`; terminal history stays
  immutable. Duration-based ends are resolved by adding invariant seconds to the resolved start instant.
- Coverage gotcha: private invariant error paths can still count against 100% line coverage. An internal
  model unit test covers the impossible-invalid `TimeZoneName` branch without exposing an invalid public
  constructor.

### 2026-06-08 — Stage 1 domain model + schedule rev
- Stage 1 precondition: re-read `notes.md`, `backlog.md`, `implementation_checklist.md`, `DESIGN.md`
  §6.3/§10/§12, and `CONVENTIONS.md`; re-ran the Stage 0/global gate and confirmed CI run
  `27128281353` was green before coding.
- TDD record: added `tests/model.rs` and `tests/properties.rs` first; the red state was the missing
  `ccplan::model` module. Later self-review tightened the tests again to require `Span` and `Run`
  domain types, catching the first implementation's too-permissive public `Option<end/duration>` and
  `Vec<String>` `run` shape.
- The public model now uses parse-don't-validate boundaries: raw TOML structs accept `end`/`duration`
  and raw `run`, then convert to `Block { span: Span::End|Span::Duration, run: Option<Run> }`.
  Missing/both span shapes and empty `run` become typed exit-code-2 validation errors at parse time.
- Unknown fields follow the locked design decision: hard reject everywhere. The checklist's
  "preserve-with-warning" phrase is stale because DESIGN §6.3 and the review notes explicitly removed
  that behavior.
- `schedule_rev` is a short blake3 hex over only `id`, start seconds, resolved end seconds, and notify
  seconds. It excludes `title`, `status`, `tags`, and `run`; equivalent `end` and `duration` timing
  produce the same rev.
- `proptest` is included with `default-features = false, features = ["std"]`; default fork/timeout
  features pulled in a duplicate `getrandom` path. `cargo tree --duplicates` is clean with the trimmed
  feature set.
- `blake3` brings transitive `arrayref` under `BSD-2-Clause`; `deny.toml` now explicitly allows that
  OSI license. No advisory or duplicate-version exceptions were added.

### 2026-06-08 — Stage 0 scaffold + CI gate
- Stage 0 started from `main` with docs only and no `Cargo.toml`; the required pre-stage DoD
  baseline failed at the missing-manifest boundary, as expected.
- Created `dev` with `git switch -c dev` before implementation.
- TDD record: added `tests/cli.rs` first; `cargo test --all-features --workspace` failed because
  `CARGO_BIN_EXE_ccplan` was unset; adding the minimal lib/bin/clap scaffold made it green.
- Recon confirmed current tooling details:
  - `dist` is the current name/CLI for cargo-dist; latest observed release is v0.32.0, while the
    Cargo subcommand remains supported. Stage 8 should use current `dist init` docs.
  - `release-plz` docs now use `actions/checkout@v6` and `dtolnay/rust-toolchain@stable`.
  - `cargo-llvm-cov` sets `cfg(coverage)` and `cfg(coverage_nightly)` itself when run on nightly,
    but the project keeps the explicit `RUSTFLAGS="--cfg coverage_nightly"` command required by the
    checklist/docs.
  - `directories` latest docs show 6.0.0 and `ProjectDirs::{data,config,state}_dir` semantics. This was
    later resolved in Stage 3 by using `directories = "6"` and allowing the transitive OSI-approved
    `MPL-2.0` license from `option-ext`.
- `cargo-deny` 0.19 uses `unmaintained = "all"` rather than a lint level; the generated template was
  used first, then tightened to deny unknown sources, wildcard dependencies, duplicate versions,
  yanked crates, and all unmaintained advisories.
- Coverage warning gotcha: with Stage 0's only `coverage(off)` use living in test-only code, nightly
  reports `feature(coverage_attribute)` as unused unless `unused_features` is allowed under the same
  `coverage_nightly` cfg. This is not a business-logic exclusion.
- CI gotchas from the first dev push: the repo-level `rust-toolchain.toml` makes bare
  `cargo llvm-cov` run under pinned stable even after `dtolnay/rust-toolchain@nightly`; use
  `cargo +nightly llvm-cov` in CI. Windows checkout converted Rust files to CRLF, which violated
  `newline_style = "Unix"`; `.gitattributes` now forces LF.
- The original pinned CI note said `codecov/codecov-action@v5`; the current Codecov README documents
  `v7` with a wrapper/key update. The v5 upload failed in CI because the uploader signature key could
  not be verified, so Stage 0 uses `codecov/codecov-action@v7`.
- Codecov v7 then reached the service with a valid OIDC token and `codecov.json`, but the service
  returned `Repository not found`; the upload step is non-blocking until the repo is activated/configured
  in Codecov. The blocking coverage quality gate remains `cargo +nightly llvm-cov --fail-under-lines
  100`.

### 2026-06-08 — review round 4 fixes + agent skill (D20)
- Final-v1 → version/tag/artifacts are **v1.0.0** (was v0.1.0) everywhere.
- Purged remaining YAML refs in DESIGN (diagram, store, `set --from`, §13) → TOML/JSON.
- Unknown-field policy unified: **reject everywhere** (read + write), `deny_unknown_fields` — no
  more "preserve-with-warning on read".
- Trigger names now carry the **full identity `date-idhash-rev-event`** on all three backends
  (systemd unit / launchd label / schtasks name) so `clear --date` is unambiguous.
- Coverage commands now set `RUSTFLAGS="--cfg coverage_nightly"` so the `coverage(off)` exclusions
  actually compile/apply (DoD, CI, CONVENTIONS, audit template, notes).
- Added `thiserror = "2"` + `anyhow = "1"` to pinned deps (error model was referenced but unpinned).
- README: "sandboxed by policy" → "policy-gated" (we don't sandbox, NG6); removed the `allow_shell`
  config key (shell never supported, NG9).
- Defined the test seam: `run(cli,out)` → `run_with_context(cli,out,&Context)`; fakes call the latter,
  `assert_cmd` reserved for parse/help/temp-store paths.
- **D20:** ship `AGENTS.md` + loadable `skills/ccplan/SKILL.md` + an agent-onboarding test
  (`tests/agent_docs.rs`) that runs the skill's documented commands so docs can't drift from the CLI;
  README now has non-interactive agent install instructions.

### 2026-06-08 — planning hardened (decisions D14–D19, conventions, Windows backend)
- Added engineering mandates: SOTA/idiomatic, `#![forbid(unsafe_code)]`, strong typing, comments=why,
  performance (D18), Don't-Make-Me-Think CLI UX (D19), no-coauthor-trailer commits (D17).
- **Windows backend changed planif → `schtasks.exe /Create /XML`** (D16) to honor "no archived dep"
  AND "no unsafe" — all three backends now uniformly shell out to the native scheduler CLI.
- Wrote `CONVENTIONS.md` (root) as the canonical coding standard (Rust API Guidelines + Style Guide +
  patterns book); checklist Coding Conventions is now its operational summary.
- Self-review of all docs: grep-audited for stray YAML / planif / COM / co-author residue — clean;
  cross-references resolve; CONVENTIONS.md wired into README + checklist + structure tree.
- Known/expected: README links to OSS files (AGENTS.md, SECURITY.md, LICENSE-MIT, etc.) that are
  created in Stage 8 — intentional forward-references describing the shipped product, not gaps.

### 2026-06-08 — project seeded
- DESIGN.md finalized through review round 3. Format switched YAML → TOML (D2). Stack pinned (§2).
- Implementation checklist + audit log + this notes file created. No code yet.
- NEXT: Stage 0 (repo & toolchain bootstrap) per `implementation_checklist.md`.
