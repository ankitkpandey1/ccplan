# ccplan — Discovered-Task Backlog

> **The dedicated place for tasks discovered *during* implementation.**
> The stage list in `implementation_checklist.md` is the **stable plan** — do not edit it to add
> work you find along the way. Instead, whenever self-reflection (per-stage rhythm phase F) surfaces
> something real but out of the current stage's scope — a refactor, a missing test, a risk, a
> follow-up, a doc gap, a better idiom found in recon — **append it here**.
>
> Then triage it: do it now if it belongs to the current stage, schedule it into a later stage by
> noting the stage, or defer it to post-`v1.0.0`. Nothing discovered is ever silently dropped.

## How to use
- Add an item with the table row format below. Newest at the bottom of the Open section.
- Give every item an ID (`B-001`, `B-002`, …), the stage it was found in, a priority
  (`P1` blocker / `P2` should-fix-before-ship / `P3` nice-to-have / `later` post-v1.0.0), and a clear
  action.
- When you act on an item, move it to **Resolved** with the commit/stage that closed it.
- Reference backlog IDs from `audit_log.md` entries when a stage raises or closes them.

## Open

| ID | Found in stage | Priority | Description / action | Target |
|----|:--------------:|:--------:|----------------------|--------|
| B-002 | 5 | P2 | Add explicit macOS/Windows dependency-policy coverage once CI can run target-aware `cargo deny` without penalizing inactive Linux-only dependencies. Current Stage 5 deny gate is scoped to `x86_64-unknown-linux-gnu`, matching the Ubuntu CI job. | Stage 8 / CI hardening |
| B-003 | 5 | P2 | Verify the macOS LaunchAgent path and Windows Task Scheduler XML on real interactive OS sessions, including notification delivery from scheduled `fire` and Windows `ccplan-fire.exe` no-console behavior. | Before v1.0.0 ship gate |
| B-004 | 5 | P3 | Decide whether manual `ccplan fire` without scheduler-injected D-Bus env should be supported on Linux. If yes, implement notification sending with an explicit bus address instead of mutating process env, because Rust 2024 makes `std::env::set_var` unsafe. | Post-Stage 6 polish |
| B-005 | 5 | P2 | Ensure Stage 8 packaging/release tooling includes both `ccplan` and the Windows `ccplan-fire` wrapper where needed. | Stage 8 release packaging |

## Resolved

| ID | Description | Closed by |
|----|-------------|-----------|
| B-001 | Decided `directories = "5"` vs `directories = "6"` for storage. Stage 3 uses `directories = "6"`; `directories 5.0.1` still pulled `option-ext` and introduced duplicate older transitive versions, while `6.0.0` keeps the duplicate graph clean. `deny.toml` explicitly allows OSI-approved `MPL-2.0` for `option-ext`. | Stage 3 implementation commits `50ef2c1` + `4afacd2` |
