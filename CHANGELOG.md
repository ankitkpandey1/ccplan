# Changelog

All notable changes to this project are documented here.

This project follows Keep a Changelog and uses Semantic Versioning.

## [Unreleased]

### Fixed

- **Fired-event ledger no longer grows without bound.** `archive` and `purge` now prune the
  fired-event keys recorded for their date. The ledger was previously append-only, so it grew
  indefinitely and every `fire` re-read and re-scanned the whole file.
- **Atomic writes now fsync the parent directory** (Unix) after the rename, so the atomic replace
  itself — not just the file contents — is durable across a crash.
- **`fire --dry-run` is now genuinely read-only.** It previews the decision a real fire would take
  without recording the at-most-once ledger entry, sending a notification, persisting the block's
  status, or writing a fire-log entry — matching the side-effect-free contract of `apply --dry-run`.
- **Notification body no longer repeats the block slug `id`.** The toast title already carries the
  human block name, so the body dropped the redundant machine slug (it rendered e.g.
  "future-focus at 11:00") and now shows only the start time ("at 11:00").

### Changed

- Reworded the `scheduler`/`notifier` "unavailable" errors to drop internal development-stage
  language ("until Stage 5") in favor of a platform-availability message.

### Security

- The `run:` automation plan-file ownership check now resolves the current UID via a safe `getuid`
  syscall wrapper instead of spawning `id -u`, removing a `PATH`-resolved subprocess from the
  security gate of a scheduler-invoked process.

## [1.0.0] - 2026-06-08

### Added

- **CLI**: Full day-planner surface — `set/add/edit/rm`, `done/skip/clear`, `show/now/next/agenda`,
  `apply/fire/status/doctor`, `completions`.  Reads return JSON arrays (`--json`); exits use
  documented codes (0/2/3/4/5/6); no interactive prompts (agent-safe).
- **Whole-plan stdin authoring**: `ccplan set --from -` reads a TOML plan from stdin, enabling
  agents to author an entire day in one shot.
- **TOML plan schema** (`date`, `timezone`, `[[block]]` array-of-tables) with `deny_unknown_fields`
  validation, `schedule_rev` keyed on trigger-affecting fields only (Inv-15), and immutable terminal
  history (Inv-7).
- **Native scheduler integration**: `systemd --user` transient timers (Linux), LaunchAgent plists
  (macOS), and Task Scheduler XML tasks (Windows).  `apply` reconciles desired vs. live triggers
  idempotently (Inv-3/Inv-10); `fire` is guarded by a durable at-most-once ledger (Inv-14).
- **Notifications**: `notify-rust` (Linux), `osascript` (macOS), PowerShell WinRT toast (Windows);
  non-fatal; non-silent on missing capability.
- **`run:` automation**: argv-only (no shell), allowlisted absolute paths, plan-file ownership
  check, configurable timeout, at-most-once via ledger, structured `fire.log` output.
- **DST-correct time resolution** via `jiff` with the Compatible ambiguity strategy.
- **Shell completions** (bash/zsh/fish/PowerShell) generated at build time and via
  `ccplan completions <shell>`.  Man page `ccplan(1)` generated at build time.
- **Agent skill** (`skills/ccplan/SKILL.md`) and canonical recipe (`AGENTS.md`) with an
  agent-onboarding test that runs the documented commands so docs cannot drift from the CLI.
- **Release automation**: `release-plz` release PR management; `cargo-dist` cross-platform binary +
  shell/PowerShell/Homebrew/MSI installers triggered on `v*.*.*` tags.
- **OSS hygiene**: dual `MIT OR Apache-2.0` license, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` (CC 2.1),
  `SECURITY.md` (run: threat model), issue templates, PR template, Dependabot.
- **Quality gate**: 100% line coverage with `#[coverage(off)]` only on sanctioned OS-IO methods;
  `clippy::pedantic`; `cargo-deny`; anti-gaming guards enforced in CI.
