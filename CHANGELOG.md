# Changelog

All notable changes to this project are documented here.

This project follows Keep a Changelog and uses Semantic Versioning.

## [Unreleased]

## [1.1.0] - 2026-06-15

### Added

- **`ccplan remind "<text>" --in <duration>`**: one-shot relative reminder. Sugar over `add` +
  `apply` — it resolves `now + duration` in the clock's time zone (minute granularity), creates a
  zero-lead block so the only alert is the `start` event firing exactly at the target, and
  auto-applies so the OS trigger goes live in one step. `--in` accepts `1h` / `30m` / `1h30m`
  (max 24h); a reminder that crosses midnight lands in the next day's plan. Same upsert and
  terminal-history rules as `add` (`--id` to override the auto-slugged id).

## [1.0.0] - 2026-06-15

The first public release.

### Added

- **CLI**: Full day-planner surface — `set/add/edit/rm`, `done/skip/clear`, `show/now/next/agenda`,
  `apply/fire/status/doctor`, `completions`.  Reads return JSON arrays (`--json`); exits use
  documented codes (0/2/3/4/5/6); no interactive prompts (agent-safe).
- **Whole-plan stdin authoring**: `ccplan set --from -` reads a TOML plan from stdin, enabling
  agents to author an entire day in one shot.
- **TOML plan schema** (`date`, `timezone`, `[[block]]` array-of-tables) with `deny_unknown_fields`
  validation, a `schedule_rev` keyed only on trigger-affecting fields, and immutable terminal
  history.
- **Native scheduler integration**: `systemd --user` transient timers (Linux), LaunchAgent plists
  (macOS), and Task Scheduler XML tasks (Windows).  `apply` reconciles desired vs. live triggers
  idempotently; `fire` is guarded by a durable at-most-once ledger.
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

### Reliability

- The fired-event ledger is pruned by `archive`/`purge` so it cannot grow without bound.
- Atomic writes fsync the parent directory (Unix) after the rename, so the atomic replace itself —
  not just the file contents — is durable across a crash.
- `fire --dry-run` is genuinely read-only: it previews the decision without recording the
  at-most-once ledger entry, sending a notification, persisting status, or writing a fire-log entry.

### Security

- The `run:` automation plan-file ownership check resolves the current UID via a safe `getuid`
  syscall wrapper instead of spawning `id -u`, removing a `PATH`-resolved subprocess from the
  security gate of a scheduler-invoked process.
</content>
</invoke>
