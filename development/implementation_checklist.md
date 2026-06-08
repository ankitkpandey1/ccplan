# ccplan — Implementation Checklist

> **This is the single, final, authoritative build plan.** Work it top to bottom. It is written so
> that *any* agent — including a low-powered one — can implement `ccplan` end to end with **zero
> ambiguity**. Do not improvise architecture: `DESIGN.md` is the spec, this file is the order of
> operations, `development/notes.md` is the memory.

---

## 🎯 GOAL

Build **ccplan**, the cross-platform (Linux · macOS · Windows) Rust CLI day-planner specified in
`DESIGN.md`, to a **production-ready, open-source-shippable `v1.0.0`**:

- Implements the full CLI surface and semantics in `DESIGN.md` (store → `apply` → `fire`; native
  schedulers; notifications; `run:` automation; lifecycle; invariants Inv-1…Inv-15).
- **100% line coverage** of the library (with only the documented `#[coverage(off)]` exclusions),
  **clippy-clean** (`-D warnings`), **fmt-clean**, **`cargo-deny`-clean**.
- Strict **TDD** throughout (red → green → refactor).
- Full **OSS hygiene** (dual license, CONTRIBUTING/COC/SECURITY/CHANGELOG, CI, templates).
- Automated **cross-platform release** producing binaries + installers (shell, PowerShell, Homebrew,
  MSI) when a version tag is pushed.

**You are done when:** `dev` is merged to `main`, tag `v1.0.0` is pushed, the release workflow
produces all platform artifacts, and every box in this file (incl. the Final Ship Gate) is checked
with a corresponding `audit_log.md` entry.

---

## ▶️ HOW TO USE THIS CHECKLIST (autonomous loop)

When invoked to "work the goal" (e.g. via `/goal`), repeat this loop until the Final Ship Gate passes:

1. **Load memory.** Read `development/notes.md` in full, then `development/backlog.md`, then this
   file. Read the `DESIGN.md` sections relevant to the current stage.
2. **Find the current stage.** The first stage below whose boxes are not all checked.
3. **Verify the previous stage (regression gate).** Re-run the previous stage's **Acceptance Gate**
   commands AND the global **Definition of Done** gate. If anything fails, **STOP and fix it before
   doing any new work** — a broken predecessor means the current stage cannot be trusted. This is how
   problems are caught immediately instead of compounding.
4. **Run the stage through the PER-STAGE RHYTHM below** (recon → implement → self-review → reflect).
5. **Check the boxes** in this file for the completed stage. Go to step 2.

### PER-STAGE RHYTHM (mandatory phases for *every* stage)

Each stage MUST proceed through these phases in order. Do not collapse or skip any.

- **A. Recon / Research** — *always start here.* Read the relevant `DESIGN.md` sections and existing
  code. Consult its **Recon focus** (table below). If the stage needs knowledge you don't already
  have verified (a crate's exact API, an OS command's flags, an edge case), **research it** (read the
  crate docs / official docs / web) **before writing any code**, and record what you learned in
  `notes.md`. Do not guess an API.
- **B. Implement (strict TDD)** — red → green → refactor, per the Test Writing Conventions. No code
  without a test driving it.
- **C. Self-review & fix** — critically review your own diff as if reviewing someone else's PR:
  correctness, every edge case, conformance to the relevant `DESIGN.md` **invariants**, error/exit
  codes, idioms, clippy. **Fix every issue you find, then re-run the tests.** Loop B↔C until your own
  review is clean.
- **D. Definition of Done gate** — run the full DoD gate (below); it must be fully green.
- **E. Self-reflect & record learnings** — write a short reflection to `notes.md` §6: what worked,
  what was tricky, what surprised you, what later stages must know. (Anti-amnesia: survives compaction.)
- **F. Capture discovered tasks** — anything you noticed that is real but **out of this stage's
  scope** (a refactor, a missing test idea, a risk, a follow-up) goes into **`development/backlog.md`**
  — the dedicated place for discovered work. **Never** inline new tasks into the stages here; the
  stage list stays the stable plan, the backlog absorbs discoveries. The agent is expected to add to
  the backlog whenever self-reflection surfaces something.
- **G. Audit entry** — write a `development/audit_log.md` entry (its template) covering: recon
  summary, what was done, exact commands + evidence, self-review findings & fixes, reflection, and
  any backlog items raised, with a checkbox-by-checkbox confirmation.
- **H. Commit** — conventional commit (see Coding Conventions). One stage = one or a few focused commits.

> **Never skip phases A (recon), C (self-review), D (gate), E (reflect), or G (audit).** They are what
> make the build correct, auditable, and self-correcting.

### Recon focus by stage (what to research/read before coding)

| Stage | Recon focus |
|------:|-------------|
| 0 | `cargo-dist`/`release-plz`/`cargo-llvm-cov`/`cargo-deny` current usage; GitHub Actions Rust matrix; `directories` layout. |
| 1 | `DESIGN.md` §6.3 schema; `toml` + `serde` derive (`deny_unknown_fields`); `blake3` hashing API. |
| 2 | `jiff` tz/DST API (`TimeZone::get`, ambiguous-time resolution strategies); `DESIGN.md` §7 event table. |
| 3 | `fs2` advisory locking; atomic write (temp+rename+fsync) idioms; `assert_fs`; `DESIGN.md` §6.2/§6.4 + Inv-7/9/14. |
| 4 | `clap` 4 derive (subcommands, value enums); `assert_cmd`/`insta`; `DESIGN.md` §8 exit codes + Inv-11; reconciler/`fire` §6/§7. |
| 5 | notes §3 platform gotchas; `systemd-run`/`launchctl`/`schtasks /Create /XML` exact invocations — **verify on the actual OS**; `notify-rust` API. |
| 6 | `DESIGN.md` §9 policy; `std::process::Command` argv exec + timeout; Unix file-perms/ownership checks. |
| 7 | `clap_complete` + `clap_mangen` `build.rs` integration. |
| 8 | `dist init` config; `release-plz` action; Contributor Covenant; keep-a-changelog; dual-license convention; **agent skill format** (SKILL.md frontmatter conventions). |
| 9 | Re-read all of `DESIGN.md` (Inv-1…Inv-15) for the conformance pass; the release/branch model (notes D13). |

---

## ✅ DEFINITION OF DONE (DoD) — the gate every stage must pass

Run from repo root. All must succeed (exit 0) before a stage is "done":

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --workspace
RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov --all-features --workspace --fail-under-lines 100   # cfg makes coverage(off) exclusions apply
cargo deny check                                                            # licenses + advisories + bans
cargo build --release                                                       # binary builds
```

If any tool isn't installed yet: `cargo install cargo-llvm-cov cargo-deny` and add the nightly
toolchain (`rustup toolchain install nightly --component llvm-tools-preview`). Stage 0 wires these
into CI so the gate runs automatically on every push.

> **Coverage honesty rule:** the only code allowed to be excluded from coverage is (a) platform
> `#[cfg(target_os=…)]` real-backend modules, (b) the real shell-out/native-API impls of `Scheduler`/
> `Notifier`/`Clock`, (c) `unreachable!()`/defensive `panic!`, (d) the `main` shim. Each exclusion
> carries `#[cfg_attr(coverage_nightly, coverage(off))]` and a one-line comment justifying it. You may
> **never** exclude business logic to hit the number.

---

## 🗂️ PROJECT STRUCTURE (canonical — create in Stage 0, keep stable)

Single Cargo package, library + thin binary. Logic lives in the lib; `main` is a shim. This layout is
fixed — put new code in the module it belongs to; don't invent top-level files ad hoc.

```
cc-planner/
├── Cargo.toml                 # edition 2024, MSRV, lints, deps (notes §2)
├── Cargo.lock                 # committed (binary crate)
├── rust-toolchain.toml        # pinned stable toolchain + rustfmt/clippy
├── rustfmt.toml  clippy.toml  deny.toml  .editorconfig  codecov.yml
├── build.rs                   # completions + man page (Stage 7)
├── src/
│   ├── main.rs                # ~10-line shim: parse → run() → ExitCode  (#[coverage(off)])
│   ├── lib.rs                 # crate attrs (see Coding Conventions); pub fn run(); module decls
│   ├── cli.rs                 # clap derive: Cli + Commands (the only arg-parsing surface)
│   ├── error.rs               # the crate Error enum (thiserror) + ExitCode mapping
│   ├── model.rs               # Plan, Block, Status, newtypes (Lead, ClockTime, BlockId …)
│   ├── time.rs                # jiff resolution; Clock trait; SystemClock/FixedClock
│   ├── lifecycle.rs           # pure decide_fire / reconcile_overdue (§7)
│   ├── store.rs               # atomic locked TOML store, archive, ledger, triggers.json
│   ├── config.rs              # config.toml model (automation/allowlist/grace…)
│   ├── context.rs             # Context{clock,scheduler,notifier,store}; Scheduler/Notifier traits + fakes
│   ├── commands/              # one module per CLI verb (set, add, show, apply, fire, …)
│   └── platform/              # real backends; each #[cfg(target_os=…)] + #[coverage(off)]
│       ├── mod.rs  systemd.rs  launchd.rs  schtasks.rs  notify.rs
├── tests/                     # integration: cli.rs, snapshots.rs, properties.rs, agent_docs.rs, integration_*.rs
│   └── snapshots/             # committed insta *.snap files
├── .github/{workflows,ISSUE_TEMPLATE}/  dependabot.yml  PULL_REQUEST_TEMPLATE.md
├── README.md  DESIGN.md  CONVENTIONS.md  AGENTS.md  CHANGELOG.md  CONTRIBUTING.md  CODE_OF_CONDUCT.md  SECURITY.md
├── skills/ccplan/SKILL.md     # loadable agent skill: install check + usage recipe (shipped)
├── LICENSE-APACHE  LICENSE-MIT
└── development/               # DEV-ONLY: notes.md, implementation_checklist.md, audit_log.md, backlog.md
```

## 🦀 CODING CONVENTIONS (non-negotiable)

> **Canonical standard: [`CONVENTIONS.md`](../CONVENTIONS.md) (repo root)**, grounded in the Rust API
> Guidelines, Style Guide, and patterns book. The list below is the operational summary — read the
> full file once before Stage 0.

- **SOTA + idiomatic Rust, edition 2024.** Use current best-practice idioms and current crate APIs
  (verify in recon — no deprecated calls). Prefer the standard library and well-maintained crates;
  if recon finds a more idiomatic approach than written here, prefer it and note the deviation.
- **No `unsafe`.** Crate root carries `#![forbid(unsafe_code)]`. Backend choices are made
  specifically to keep it that way — e.g. Windows scheduling shells out to `schtasks.exe` rather than
  using `windows`-rs COM (which would require `unsafe`). If a future dependency ever forces an unsafe
  boundary, it stays **inside that dependency**, never in our code, and is documented in `notes.md`.
  No scenario in this project needs `unsafe`.
- **Strong type safety — make illegal states unrepresentable.**
  - **Newtypes over primitives:** `BlockId(String)`, `Lead(Duration)`, `ClockTime`, `Rev([u8;32])` —
    never pass bare `String`/`u64` where a domain type exists.
  - **Enums over stringly/boolean typing:** `Status`, `Event{Notify,Start,End}`, `FireDecision` — no
    "magic string" statuses, no boolean params that need a comment to decode.
  - **Parse, don't validate:** convert untrusted input (TOML, args) into validated domain types at the
    boundary; downstream code receives only already-valid types. `end`-vs-`duration` becomes one enum
    (`Span::End|Span::Duration`), so "both set" is unrepresentable, not a runtime check.
  - Non-empty argv for `run:` modeled so emptiness can't occur downstream.
  - Avoid `unwrap()`/`expect()`/`panic!` in library code paths except for true invariants that are
    `unreachable!` by construction (and mark those `#[coverage(off)]`). `main`/tests may `expect`.
- **Errors:** one crate `Error` enum via `thiserror`; `Result<T, Error>` everywhere in the lib;
  `anyhow` only at the `main` boundary if helpful. Each error variant maps to a documented exit code
  (`error.rs`). Use `?`; never swallow errors.
- **Clippy:** `cargo clippy --all-targets --all-features -- -D warnings`, and enable
  `clippy::pedantic` (allow-list only the few lints that are genuinely noisy, each with a reason).
- **Crate root attributes** (`lib.rs`):
  ```rust
  #![forbid(unsafe_code)]
  #![cfg_attr(coverage_nightly, feature(coverage_attribute))]
  #![warn(clippy::pedantic)]
  ```
- **Comments explain WHY, never WHAT.** A comment states a design decision, a trade-off, an invariant,
  or a non-obvious constraint ("rev excludes status so the end-trigger stays valid — Inv-15"). It must
  **never** restate what the code already says ("// loop over blocks"). If a comment paraphrases the
  code, delete it and let the code speak; if the code needs explaining, rename things instead.
  `///` doc-comments on public items describe contract/behavior for the reader, which is allowed and
  encouraged — that's API documentation, not what-narration.
- **Performance is a feature** (D18): fast startup (the binary is a many-times-a-day fire target),
  borrow over clone, allocate deliberately, no needless blocking on the `fire` hot path — without
  sacrificing clarity of pure logic. See `CONVENTIONS.md` §6.
- **"Don't Make Me Think" CLI UX** (D19): obvious/consistent commands + flags, good `--help`
  everywhere, sensible defaults so the common path needs almost no flags, and error messages that say
  what's wrong **and** how to fix it. No prompts (agent-safe). See `CONVENTIONS.md` §8.
- **Formatting:** `cargo fmt` is the only authority; never hand-format. `.editorconfig` mirrors it.
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org) — `feat:`/`fix:`/`test:`/
  `refactor:`/`docs:`/`chore:`/`ci:`; breaking → `feat!:`. Drives `release-plz` + the changelog.
  **No commit trailers** — no `Co-Authored-By`, no "Generated with…" lines. Clean messages only.
- **Branch:** all work on `dev`; never commit to `main` before the Final Ship Gate. Each stage ends
  green and committed — never leave the tree red between stages.

## 🧩 PATTERNS (use these; don't reinvent)

- **Dependency injection via traits** (the backbone of testability): side-effecting capabilities are
  traits — `Clock`, `Scheduler`, `Notifier` — held in a `Context` struct threaded through `run`.
  Production wires real impls; tests wire `FixedClock` + recording fakes. Prefer generics
  (`<S: Scheduler>`, zero-cost) or `&dyn` where object-safety/binary-size matters. Real impls are the
  only `#[coverage(off)]` code.
- **Pure core, imperative shell:** keep decision logic (`model`, `time`, `lifecycle`) pure and
  total — input → output, no IO. Push IO (`store`, `platform`) to the edges. Pure functions are
  trivially 100%-testable and property-testable.
- **Parse-don't-validate at the boundary** (see type safety above): `cli.rs`/`store.rs` produce
  validated domain types; the rest of the code never re-checks.
- **Newtype + `TryFrom`** for every parsed scalar (`ClockTime::try_from("11:00")`, `Lead::try_from
  ("30m")`), with the parse logic and its tests co-located.
- **`#[non_exhaustive]`** on public enums/structs that may grow (e.g. `Error`, config), so additions
  aren't breaking.
- **Builder/typestate** only where it genuinely prevents misuse; don't over-engineer.
- **Determinism:** never call `Zoned::now()` / spawn a process / read `$HOME` directly in logic —
  always go through the injected `Clock`/`Scheduler`/`Store`(base-dir). This is a hard rule.

## 🧪 TEST WRITING CONVENTIONS

- **TDD always:** write the failing test first (red), minimal code to pass (green), then refactor.
  No production line exists without a test that drove it.
- **Placement:** white-box unit tests inline as `#[cfg(test)] mod tests` next to the code (mark the
  module `#[cfg_attr(coverage_nightly, coverage(off))]`); black-box tests in `tests/` — `cli.rs`
  (`assert_cmd` + `assert_fs` over the real binary), `snapshots.rs` (`insta`), `properties.rs`
  (`proptest`), `integration_*.rs` (`#[ignore]` real-OS, per-OS CI).
- **One behavior per test; name says the behavior** — `fire_start_overdue_marks_missed`, not `test1`.
  Arrange-Act-Assert structure.
- **No real side effects in unit/e2e tests:** inject `FixedClock` + recording fakes; use
  `assert_fs::TempDir` for all filesystem work — tests must never touch the user's real plans, real
  systemd/launchd, the real clock, or the network. Tests must be deterministic and order-independent.
- **Snapshot tests** (`insta`) for any structured/`--json`/rendered output; redact nondeterministic
  fields (timestamps, hashes); review with `cargo insta review`; commit `.snap` files; CI runs with
  `INSTA_UPDATE=no` so unreviewed snapshots fail.
- **Property tests** (`proptest`) for invariants, not examples: TOML round-trip, `schedule_rev`
  stability under reordering, `apply` idempotence, ledger check-and-set idempotence, "terminal status
  never transitions." Add the relevant invariant each stage introduces.
- **Invariant traceability:** by Stage 9 every `DESIGN.md` invariant Inv-1…Inv-15 must have at least
  one named test; reference the invariant in the test name or a `///`-doc.
- **Coverage maintained, not retrofitted:** run
  `RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov --fail-under-lines 100` each stage so the
  number never regresses. The only legitimate exclusions are listed in the DoD gate's
  coverage-honesty rule.

---

## 📦 STAGES

Each stage is self-contained, builds strictly on the previous one, and is independently auditable.
Tick every box. `[ ]` → `[x]` only when true *and* evidenced in the audit log.

---

### Stage 0 — Repo, toolchain & CI bootstrap

**Objective:** A buildable lib+bin skeleton on `dev`, with the full quality gate running in CI from
commit one, so every later stage is automatically regression-checked.

**Why first:** the gate must exist before there's code to gate. Everything downstream relies on it.

**Preconditions:** none (first stage). Confirm you're in the repo, on a fresh `dev` branch
(`git switch -c dev`).

**Steps:**
- [x] `git switch -c dev`.
- [x] Create the Cargo package: single package with both a library and a binary target. `Cargo.toml`
      with `edition = "2024"`, `rust-version = "1.85"`, `license = "MIT OR Apache-2.0"`, repository/
      description metadata, the `[lints.rust] unexpected_cfgs` line (notes §2), and the dependency
      block from `notes.md` §2 (add deps as stages need them; you may start with `clap`, `serde`,
      `toml`, `serde_json`).
- [x] `src/lib.rs`: add crate-root attr `#![cfg_attr(coverage_nightly, feature(coverage_attribute))]`
      and a stub `pub fn run(...) -> anyhow::Result<()>` (or a typed error) returning `Ok(())`. Export
      a `cli` module with a minimal `clap` `Cli` (just `--version`/`--help` working).
- [x] `src/main.rs`: the ~10-line shim (parse args → `run` → `ExitCode`), marked
      `#[cfg_attr(coverage_nightly, coverage(off))]` with a justifying comment.
- [x] `rust-toolchain.toml` (channel = latest stable, components rustfmt+clippy), `.gitignore`
      (`/target`, `Cargo.lock` **kept** — it's a binary), `.editorconfig`, `rustfmt.toml`, `clippy.toml`.
- [x] `deny.toml` (`cargo deny init` then tighten: deny unknown licenses; allow `MIT`, `Apache-2.0`,
      `Unicode-3.0`, etc.; advisories deny vulnerabilities + `unmaintained`). The dependency set is
      chosen to be advisory-clean, so no `unmaintained` allow-list entries should be needed; if one
      becomes necessary, it must be justified in a comment and raised in `backlog.md`.
- [x] `.github/workflows/ci.yml`: matrix `{ubuntu, macos, windows}` running fmt-check, clippy `-D
      warnings`, `cargo test`; a separate MSRV job (`dtolnay/rust-toolchain@1.85.0` → `cargo check`);
      a coverage job (`RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov --fail-under-lines
      100` → upload to Codecov; `check-cfg` already lists `coverage_nightly`); a `cargo-deny` job.
      Use `Swatinem/rust-cache@v2`, `taiki-e/install-action` for tools. (Versions in notes §2.)
- [x] `.github/dependabot.yml` for `cargo` + `github-actions`, weekly.
- [x] One e2e test in `tests/cli.rs`: `ccplan --version` exits 0 and prints a version
      (`assert_cmd` + `predicates`).

**Acceptance Gate:**
- [x] DoD gate passes locally (coverage trivially 100% — only the covered stub + excluded shim/main).
- [ ] `git push -u origin dev` and **CI is green on all three OSes**.
- [ ] Audit entry written; `notes.md` running log updated.

**Commit:** `chore: scaffold lib+bin, toolchain, and CI quality gate`

---

### Stage 1 — Domain model & TOML schema (pure, no IO)

**Objective:** The data model and its (de)serialization + validation + schedule-`rev`, with nothing
touching the filesystem, clock, or OS yet.

**Preconditions:** Stage 0 gate green (run DoD + CI check).

**Steps (TDD each):**
- [ ] Types in `src/model.rs`: `Plan { date, timezone, blocks: Vec<Block> }`,
      `Block { id, title, start, end|duration, notify, tags, status, run }`,
      `Status` enum (`Pending|Active|Done|Skipped|Missed|Expired`), a `Lead`/`Duration` newtype
      parsing `"30m"`, `"90s"`, `"1h30m"`, and a wall-clock `ClockTime` parsing `"11:00"`.
- [ ] `serde` derive + TOML (de)serialize matching `DESIGN.md` §6.3 exactly (array-of-tables
      `[[block]]`). **Reject unknown fields on write/parse** (`#[serde(deny_unknown_fields)]`),
      preserve-with-warning on read where the design says so.
- [ ] Validation (`Plan::validate`): unique `id` per day; exactly one of `end`/`duration`; well-formed
      times; argv non-empty if `run` present. Typed errors mapping to exit code `2`.
- [ ] **Schedule `rev`** (`Block::schedule_rev`): blake3 over the canonicalized trigger-affecting
      fields ONLY (`id`, resolved `start`, resolved `end`/`duration`, `notify`) — never status/title/
      tags/run (D5, Inv-15). Deterministic and order-independent at the plan level.
- [ ] Unit tests: parse/serialize fixtures; each validation error; rev excludes status/title/run; rev
      changes when timing changes.
- [ ] `proptest` invariants in `tests/properties.rs`: (a) TOML round-trip `parse(write(p)) == p`;
      (b) `schedule_rev` stable under block reordering; (c) editing `status`/`title`/`run` leaves
      `schedule_rev` unchanged.

**Acceptance Gate:**
- [ ] DoD green; `model` module at 100% coverage. Audit + notes updated.

**Commit:** `feat: plan/block model with TOML schema, validation, and schedule-rev`

---

### Stage 2 — Time resolution, Clock trait & lifecycle logic (pure)

**Objective:** Resolve wall-clock + date + timezone → absolute instants (DST-correct), define the
injectable `Clock`, and implement the lifecycle/fire **decision** logic as pure functions.

**Preconditions:** Stage 1 gate green.

**Steps (TDD each):**
- [ ] Add `jiff` (notes §2). `src/time.rs`: `resolve(date, tz, wallclock) -> jiff::Timestamp` using
      jiff's ambiguous-time handling (Compatible strategy; document the choice). Tests cover a normal
      time, a spring-forward gap, and a fall-back fold for a real DST zone (e.g. `America/New_York`).
- [ ] `Clock` trait (`fn now(&self) -> jiff::Zoned`). `SystemClock` (real, `#[coverage(off)]`) and
      `FixedClock` (test, in `#[cfg(any(test, feature="test-fakes"))]`).
- [ ] `src/lifecycle.rs`: pure `decide_fire(block, event, scheduled_at, now, grace) -> FireDecision`
      implementing the §7 event table exactly: `notify` overdue → `NoOp`; on-time → `Notify`; `start`
      on-time → `Activate{notify, maybe_run}`, overdue+pending → `MarkMissed`; `end` on-time/overdue
      while active → `Close(done|expired)`, already-terminal → `NoOp`. And `reconcile_overdue(plan,
      now)` → status updates (the "next apply/query reconcile" path).
- [ ] Unit tests for every cell of the §7 table + grace boundary (just inside / just outside).
- [ ] `proptest`: a block never transitions out of a terminal state; `decide_fire` is a pure function
      of its inputs (no hidden state).

**Acceptance Gate:**
- [ ] DoD green; `time` + `lifecycle` at 100% (minus `SystemClock`). Audit + notes updated.

**Commit:** `feat: DST-correct time resolution, Clock trait, and lifecycle decision logic`

---

### Stage 3 — Storage layer (filesystem, injected root)

**Objective:** Atomic, locked, validated persistence of plans + archive + the fired-event ledger +
triggers bookkeeping, all under an injectable base directory so tests use a temp dir.

**Preconditions:** Stage 2 gate green.

**Steps (TDD each):**
- [ ] Add `directories`, `fs2`, `blake3`. `src/store.rs`: a `Store` that takes a base dir (real =
      `directories` data/config/state; test = `assert_fs::TempDir`). Paths per `DESIGN.md` §6.2 (TOML).
- [ ] Atomic write: serialize → write temp file → `fsync` → rename; guard with an `fs2` lockfile
      (Inv-9). Concurrent-writer test: second lock attempt fails clean.
- [ ] `load_plan(date)` validates on read and **rejects invalid plans** (covers hand edits, §11/§12).
- [ ] `set` merge semantics (Inv-7/D9): incoming non-terminal blocks replace; terminal blocks always
      retained even if omitted; reusing/altering a terminal id without override → error (exit 6).
- [ ] `archive(date)` (move plan to `archive/`) and `purge(date)`.
- [ ] **Fired-event ledger** (`fired.json`): atomic check-and-set keyed `(date,id,event,rev,
      scheduled_at)` returning "already fired?" — under the same lock (D7, Inv-14, §6.4).
- [ ] `triggers.json`: record/list/remove owned trigger descriptors + the rev each was built from.
- [ ] Unit/integration tests with `assert_fs`: round-trip; atomic-write survives simulated crash
      (temp left, real intact); ledger dedup; set-merge retains terminal blocks; reject invalid file.
- [ ] `proptest`: ledger check-and-set is idempotent (second set of same key returns "already").

**Acceptance Gate:**
- [ ] DoD green; `store` at 100%. Audit + notes updated.

**Commit:** `feat: atomic locked TOML store with archive, fired-ledger, and trigger bookkeeping`

---

### Stage 4 — CLI surface & command logic (wired with fakes)

**Objective:** Every user-facing command implemented over the store + lifecycle, plus the `apply`
reconciler and the `fire` handler — all exercised through a `Scheduler`/`Notifier` **fake** so no
real OS scheduling happens. This is the first stage where the product is usable end-to-end (with a
fake backend).

**Preconditions:** Stage 3 gate green.

**Steps (TDD each):**
- [ ] `src/context.rs`: `Scheduler` + `Notifier` traits; `Context { clock, scheduler, notifier,
      store }`. Recording fakes (`RecordingScheduler`, `RecordingNotifier`) in the test-fakes module.
- [ ] **Define the test seam (two entrypoints):** `pub fn run(cli, out) -> Result<()>` builds the
      *real* `Context` and delegates to `pub fn run_with_context(cli, out, &Context) -> Result<()>`,
      which holds all dispatch logic. Fake-backed integration tests call `run_with_context` with
      recording fakes over an `assert_fs::TempDir`; the real `run` is only exercised by the binary.
- [ ] `src/cli.rs`: full `clap` derive tree for every command in `DESIGN.md` §8 (`set/add/edit/rm/
      done/skip/clear/show/now/next/agenda/apply/status/doctor/fire/completions`), with the exact
      flags, `--json`, `--yes`, `--override-history`, `--dry-run`.
- [ ] `run_with_context` dispatch: implement each command's logic. Reads emit human output and, with
      `--json`, stable JSON; multi-match reads (`now/next/agenda`) emit JSON **arrays** (Inv-11). Map
      all errors to the documented exit codes (`2/3/4/5/6`).
- [ ] `apply` = the **reconciler**: compute desired triggers (notify/start/end per block, future only)
      vs `triggers.json`; call `scheduler.add/remove` to converge (idempotent, Inv-3); import session
      env note deferred to Stage 5's real backend. `--dry-run` prints the diff without calling the scheduler.
- [ ] `clear` calls the **same reconciler path** to remove that day's triggers (consistency fix), then archives.
- [ ] `fire` handler: ledger check-and-set → `decide_fire` (§7) → notify/run(stub until Stage 6)/close
      → persist status → append `fire.log`. Stale rev / already-fired → no-op.
- [ ] Tests: fake-backed integration tests call **`run_with_context`** (recording fakes + temp store)
      and assert the fakes recorded the expected add/remove/notify calls; `insta` snapshots of `--json`
      output (with redactions for timestamps). Reserve **`assert_cmd`** (real binary) for parse/`--help`/
      exit-code paths and temp-store reads that don't need a fake scheduler.
- [ ] `proptest`: `apply` is idempotent (apply twice ⇒ identical recorded trigger set).

**Acceptance Gate:**
- [ ] DoD green; command logic + reconciler + fire at 100% (backends still faked). Audit + notes updated.

**Commit:** `feat: full CLI surface, apply reconciler, and fire handler (fake backend)`

---

### Stage 5 — Native scheduler & notifier backends (per-OS)

**Objective:** Real `Scheduler` + `Notifier` implementations for Linux/macOS/Windows behind the
traits, plus `doctor`. These are the only modules excluded from coverage.

**Preconditions:** Stage 4 gate green.

**Steps:**
- [ ] `src/platform/mod.rs` selects the backend by `#[cfg(target_os=…)]`. Each backend module is
      `#[cfg_attr(coverage_nightly, coverage(off))]` with a justifying comment.
- [ ] **Linux** (`platform/systemd.rs`): shell out to `systemd-run --user` exactly per notes §3
      (unit name `ccplan-<date>-<idhash>-<rev>-<event>` — full trigger identity, `--on-calendar`, `AccuracySec=1s`, `--setenv` for
      `DBUS_SESSION_BUS_ADDRESS`/`DISPLAY`; absolute binary path; `systemd-analyze calendar` validation;
      list via `systemctl --user list-timers 'ccplan-*'`; cancel via `stop`; idempotent stop-then-run).
- [ ] **macOS** (`platform/launchd.rs`): author plist via `plist` crate, `launchctl bootstrap`/
      `bootout`; one-shot self-destruct in the `fire` path (bootout own label + delete plist). No `RunAtLoad`.
- [ ] **Windows** (`platform/schtasks.rs`): shell out to `schtasks.exe /Create /XML <tmpfile>` with a
      TimeTrigger (`StartBoundary`, second precision) under the `\ccplan\` task folder; `EndBoundary` +
      `DeleteExpiredTaskAfter=PT0S` for auto-cleanup; `<Hidden>` + a `windows_subsystem = "windows"`
      fire shim so no console flashes; list/delete via `schtasks /Query|/Delete`. No `planif`/COM, no
      `unsafe` (notes §3, D16).
- [ ] **Notifier** (`platform/notify.rs`): `notify-rust` title+body; failure logged + non-fatal.
- [ ] `doctor`: detect the native scheduler (is `systemd --user` up? `launchctl` domain? Task
      Scheduler reachable?) + notifier capability + timezone, and print actionable fixes. Notification
      capability unavailable ⇒ loud warning (never silent). `doctor` logic that's testable (parsing/
      formatting) is unit-tested; the probes themselves are in the `#[coverage(off)]` backend.
- [ ] **Integration tests** (`tests/integration_*.rs`, `#[ignore]` by default, run per-OS in CI with a
      dedicated job or `--features integration`): on Linux, actually `apply` a near-future block, assert
      a `ccplan-*` timer exists, then `clear` removes it. Keep minimal; document flakiness in notes.

**Acceptance Gate:**
- [ ] DoD green (coverage still 100% because backends are `coverage(off)`; verify the exclusions are
      ONLY the listed ones). CI matrix green on all three OSes. Manually dogfood on the Linux dev box:
      `apply` a block 1 minute out and confirm a real desktop notification fires. Audit + notes updated.

**Commit:** `feat: native systemd/launchd/Task-Scheduler backends, notifier, and doctor`

---

### Stage 6 — `run:` automation & security

**Objective:** Execute a block's `run:` command at fire time, under the full §9 policy, at most once.

**Preconditions:** Stage 5 gate green.

**Steps (TDD each):**
- [ ] `config.toml` model (`directories` config dir): `automation.enabled` (default false),
      `allowed_executables` (absolute paths), `timeout` (default `5m`), `notify.default_lead`, `grace`.
- [ ] In `fire`: if block has `run`, enforce — automation enabled? argv[0] absolute + on allowlist?
      plan file owned-by-user + not world-writable? Else exit `5` + log; never run.
- [ ] Execute as an **argv vector, no shell**; capture exit/stdout/stderr tail; enforce `timeout`
      (kill on overrun); append a structured line to `fire.log`. At-most-once already guaranteed by the
      ledger (Stage 3) — assert it: a duplicate `fire` does not re-run.
- [ ] `--dry-run` on `apply`/`fire` prints the command without executing.
- [ ] Tests: policy matrix (disabled / not-allowlisted / not-absolute / bad-perms → refused; allowed →
      runs). Execute a harmless real binary in tests (e.g. the platform's `true`/`cmd /c exit 0`, or a
      tiny test helper bin), never arbitrary input. Timeout test with a sleeping helper. Ownership/perms
      test via `assert_fs` (Unix perms `#[cfg(unix)]`-gated). Duplicate-fire no-op test.

**Acceptance Gate:**
- [ ] DoD green; automation policy logic at 100%. Audit + notes updated.

**Commit:** `feat: allow-listed, no-shell, at-most-once run: automation with timeout and logging`

---

### Stage 7 — CLI niceties: completions & man page

**Objective:** Generate shell completions and a man page, packaged for installers.

**Preconditions:** Stage 6 gate green.

**Steps:**
- [ ] Add `clap_complete` + `clap_mangen` as build-deps. Refactor the `clap` command into a file
      `build.rs` can `include!` (or a `CommandFactory` reachable from build).
- [ ] `build.rs`: generate bash/zsh/fish/PowerShell completions + `ccplan.1` into `OUT_DIR`.
- [ ] `ccplan completions <shell>` runtime subcommand (prints to stdout) as a fallback.
- [ ] Tests: `assert_cmd` — `ccplan completions bash` exits 0 and emits a non-empty script for each shell.

**Acceptance Gate:**
- [ ] DoD green. Audit + notes updated.

**Commit:** `feat: shell completions and man page generation`

---

### Stage 8 — OSS hygiene, agent skill & release engineering

**Objective:** Make the repo a polished, releasable open-source project — including a **shipped,
tested agent skill** so agents can install and use `ccplan` — with automated cross-platform release.

**Preconditions:** Stage 7 gate green.

**Steps:**
- [ ] **Dual license:** rename existing `LICENSE` → `LICENSE-APACHE`; add `LICENSE-MIT`; confirm
      `Cargo.toml` `license = "MIT OR Apache-2.0"`.
- [ ] OSS files: `CONTRIBUTING.md` (conventional commits, how to run the DoD gate, **links to
      `CONVENTIONS.md`** as the coding standard),
      `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `SECURITY.md` (the `run:` threat model + private
      reporting), `CHANGELOG.md` (keep-a-changelog header), `.github/ISSUE_TEMPLATE/{bug,feature}.yml`,
      `.github/PULL_REQUEST_TEMPLATE.md`.
- [ ] **Agent skill + AGENTS.md (D20):** write `AGENTS.md` (canonical recipe) and the loadable skill
      `skills/ccplan/SKILL.md` with valid frontmatter (`name`, `description`, when-to-use) covering:
      non-interactive install (`cargo binstall ccplan` / shell installer) + `ccplan --version` + `ccplan
      doctor` verification, the `set --from -` → `apply` recipe, the exit-code table, and the `--json`
      array contract. Keep the skill's commands copy-pasteable and current with the real CLI.
- [ ] **Agent-onboarding test** (`tests/agent_docs.rs`): (a) parse `skills/ccplan/SKILL.md` frontmatter
      and assert required fields exist; (b) extract the recipe's `ccplan …` commands and **run them via
      `assert_cmd` against a temp store** (with a fake/headless scheduler) asserting success + expected
      JSON — so the documented agent flow can never silently drift from the real CLI. Keep `AGENTS.md`
      and `SKILL.md` in sync (one is the source, the other generated/checked, or a test asserts they match).
- [ ] **release-plz** workflow (`.github/workflows/release-plz.yml`) on `main` — maintains the release
      PR + tags on merge (notes §2 versions).
- [ ] **dist / cargo-dist** (`dist init`): configure `[workspace.metadata.dist]` with installers
      `["shell","powershell","homebrew","msi"]`, the five targets, `tap` for Homebrew; generate
      `.github/workflows/release.yml` (triggers on `v*.*.*` tag). Add `[package.metadata.binstall]` for
      `cargo binstall`. Have the release package the completions + man page.
- [ ] README badges point at the real CI/coverage/crates/license once IDs exist.
- [ ] `cargo-deny` clean with **no** `unmaintained`/advisory allow-list entries needed (dependency set is advisory-clean).

**Acceptance Gate:**
- [ ] DoD green. `dist plan` succeeds locally (dry plan of artifacts). `release-plz` workflow validates.
      Audit + notes updated.

**Commit:** `chore: dual license, OSS docs, and automated cross-platform release pipeline`

---

### Stage 9 — Production readiness & ship `v1.0.0`

**Objective:** Final verification, dogfood, and the actual release.

**Preconditions:** Stage 8 gate green; **all** prior stage boxes checked with audit entries.

**Steps:**
- [ ] **Full regression:** run the DoD gate on a clean checkout; confirm CI green on all three OSes;
      confirm coverage is a true 100% and the `coverage(off)` set contains ONLY the sanctioned exclusions.
- [ ] **Dogfood end-to-end** on the dev machine: author a real day with `set --from -`, `apply`,
      confirm notifications fire at the right times, an allow-listed `run:` executes once, `done`/`now`/
      `next` behave, `clear` cleans up triggers. Record evidence in the audit log.
- [ ] **Spec conformance pass:** walk `DESIGN.md` invariants Inv-1…Inv-15 and confirm a test exists for
      each; list the test name per invariant in the audit entry.
- [ ] Finalize `CHANGELOG.md` for `1.0.0`; ensure `version = "1.0.0"`.
- [ ] **Ship:** open PR `dev` → `main`; CI green; merge. `release-plz` opens/!updates the release PR;
      merging it pushes tag `v1.0.0` → `release.yml` builds binaries + shell/PowerShell/Homebrew/MSI
      installers + checksums and publishes the GitHub Release.
- [ ] Verify the published release: download one artifact per OS and smoke-test `ccplan --version`.

**Final Ship Gate:**
- [ ] DoD green on clean checkout; CI green on Linux+macOS+Windows.
- [ ] 100% coverage; only sanctioned exclusions.
- [ ] Every Inv-1…Inv-15 has a named test.
- [ ] Dogfood evidence recorded.
- [ ] `v1.0.0` tagged; release workflow produced all platform artifacts; artifacts smoke-tested.
- [ ] Final audit entry written summarizing the whole build.

**Commit / tag:** merge PR → `release-plz` tags `v1.0.0`.

---

## 🧹 Post-ship (optional)

- [ ] Consider removing or git-ignoring `development/` from published artifacts (it's dev-only).
- [ ] Triage "later" features (notification action buttons — Linux-first; status-line integration;
      calendar import; daily templates) into issues. Do **not** expand `v1.0.0` scope.

---

## 📍 Progress tracker (update as you go)

| Stage | Title | Done | Audit entry | Commit |
|------:|-------|:----:|:-----------:|--------|
| 0 | Repo/toolchain/CI bootstrap | [ ] | | |
| 1 | Domain model & TOML schema | [ ] | | |
| 2 | Time, Clock, lifecycle logic | [ ] | | |
| 3 | Storage layer | [ ] | | |
| 4 | CLI surface & command logic | [ ] | | |
| 5 | Native backends & doctor | [ ] | | |
| 6 | run: automation & security | [ ] | | |
| 7 | Completions & man page | [ ] | | |
| 8 | OSS hygiene & release | [ ] | | |
| 9 | Production readiness & ship | [ ] | | |
