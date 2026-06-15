<div align="center">

# ccplan

**A plain-text, agent-fillable day planner that notifies you, tracks block status, and runs commands at the right time.**

[![CI](https://github.com/ankitkpandey1/cc-planner/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/ankitkpandey1/cc-planner/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ccplan.svg)](https://crates.io/crates/ccplan)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

</div>

> **Status:** pre-release. The `1.0.0` code is complete, but it is not yet tagged or published —
> install by [building from source](#build-from-source) for now. Design notes live in [`DESIGN.md`](DESIGN.md).

---

## What is ccplan?

`ccplan` is a cross-platform command-line tool for planning your day as a set of **time blocks**.
You — or a coding agent like Claude Code — write a plain-text plan, and `ccplan` turns it into
real, native OS-scheduled events that:

- **Alert you** with a desktop notification at each block (and an optional lead time),
- let you **mark blocks done / skipped**,
- and optionally **run a command** when a block starts (start a sync, kick off a build, open a doc).

It is built to be driven by an agent: every command is non-interactive, scriptable, and emits
JSON, so an agent can plan your day end to end.

### Why not a calendar / to-do app / cron?

| | Agent can fill it | Desktop alerts | Mark done | Run a command at a time | Cross-platform |
|------|:---:|:---:|:---:|:---:|:---:|
| Google Tasks | ✗ (no real CLI) | partial | ✓ | ✗ | ✓ |
| Calendar / CalDAV | ~ | ✓ | ~ | ✗ | ~ |
| Taskwarrior | ✓ | via glue | ✓ | ✗ (hooks fire on *edit*, not at a *time*) | ✓ |
| cron / systemd / launchd | ✓ | — | — | ✓ | per-OS |
| **ccplan** | ✓ | ✓ | ✓ | ✓ | ✓ |

`ccplan` is the one tool that is agent-authorable, time-triggered, can both notify **and** execute,
runs on Linux/macOS/Windows, and keeps a human-readable plain-text plan as its source of truth.

---

## Features

- **Plain-text plan** — one human- and agent-editable [TOML](#the-plan-file) file per day.
- **Agent-first CLI** — non-interactive, stable exit codes, `--json` on every read, whole-day authoring from stdin.
- **Native notifications** — at block start and at optional per-block lead times.
- **Status tracking** — `done` / `skipped`, with automatic `missed` / `expired` detection.
- **Per-block automation** — run an allow-listed command when a block fires (opt-in, policy-gated).
- **Truly cross-platform** — uses the native scheduler on each OS (systemd / launchd / Task Scheduler). No background daemon.
- **Idempotent & safe** — re-apply converges; atomic writes; at-most-once firing; immutable history.
- **Local & offline** — no account, no network, no telemetry.

---

## Install

> Prebuilt binaries and installers are produced by the release pipeline starting at `v1.0.0`.
> Until then, [build from source](#build-from-source).

```sh
# Linux / macOS (shell installer)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/ankitkpandey1/cc-planner/releases/latest/download/ccplan-installer.sh | sh

# Windows (PowerShell installer)
powershell -c "irm https://github.com/ankitkpandey1/cc-planner/releases/latest/download/ccplan-installer.ps1 | iex"

# Homebrew
brew install ankitkpandey1/tap/ccplan

# Cargo (from crates.io, once published)
cargo install ccplan
# or prebuilt via binstall
cargo binstall ccplan
```

---

## Quickstart

```sh
# 1. Add a few blocks to today's plan
ccplan add --title "Focus time"      --start 11:00 --end 11:30
ccplan add --title "Agentic sync-up" --start 11:30 --duration 30m --notify 2m
ccplan add --title "Standup"         --start 16:25 --duration 5m

# 2. See your day
ccplan show

# 3. Schedule the alerts with the OS
ccplan apply

# 4. As the day goes on
ccplan now              # what's active right now
ccplan next             # what's coming up
ccplan done focus-time  # mark a block complete (id is auto-slugged from the title)
```

That's it — at 11:00 you get a "Focus time" notification, at 11:30 the sync-up alert fires, and so on.

---

## For agents

`ccplan` is designed to be filled in by an AI agent.

**Install (agent-driven).** An agent can install `ccplan` non-interactively and confirm it works:

```sh
# Install the binary (pick what's available in the environment)
cargo binstall -y ccplan  ||  cargo install ccplan  ||  \
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/ankitkpandey1/cc-planner/releases/latest/download/ccplan-installer.sh | sh

ccplan --version          # confirm install
ccplan doctor             # confirm the OS scheduler + notifier are usable, with fixes if not
```

**Load the skill.** The repo ships a ready-to-use agent skill at
[`skills/ccplan/SKILL.md`](skills/ccplan/SKILL.md). Drop it into your agent's skills directory
(for Claude Code: `cp -r skills/ccplan ~/.claude/skills/`) so the agent knows when and how to use
`ccplan` — installation check, the authoring recipe, exit codes, and the JSON contract. A short
[`AGENTS.md`](AGENTS.md) at the repo root carries the same canonical recipe inline.

**Canonical recipe** — **author the whole day, then apply**:

```sh
# An agent composes the full day as TOML and pipes it in, then schedules it.
ccplan set --from - <<'TOML'
date     = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
end   = "11:30"

[[block]]
id = "sync-1"
title = "Agentic sync-up"
start = "11:30"
duration = "30m"
notify = "2m"
run = ["/home/me/bin/sync.sh", "--fast"]
TOML

ccplan apply
```

Agent-friendly guarantees:

- **Never interactive** — no prompts; destructive operations require an explicit flag (`--yes`, `--override-history`).
- **`--json` on every read** — `show`, `now`, `next`, `agenda` emit stable JSON; queries that can match
  multiple blocks always return a JSON **array** (empty is `[]`, never an error).
- **Deterministic exit codes** — `0` ok · `2` usage/validation · `3` not found · `4` scheduler failure ·
  `5` automation refused (policy/allowlist) · `6` history conflict (needs `--override-history`).
- **Whole-plan I/O** — `ccplan set --from -` reads a full day from stdin; `ccplan show --json` round-trips it.

Both [`AGENTS.md`](AGENTS.md) and the [`skills/ccplan/SKILL.md`](skills/ccplan/SKILL.md) skill ship
with the project and document this recipe for any agent.

---

## The plan file

One [TOML](https://toml.io) file per day, stored under your OS data dir
(`~/.local/share/ccplan/plans/YYYY-MM-DD.toml` on Linux). You can edit it by hand or let an agent
write it; either way `ccplan apply` re-validates it before scheduling.

```toml
date     = "2026-06-08"
timezone = "Asia/Kolkata"          # frozen at author time; all times resolve against this

[[block]]
id     = "focus-1"                 # stable, unique within the day
title  = "Focus time"
start  = "11:00"                   # local wall-clock
end    = "11:30"                   # OR  duration = "30m"  (exactly one)
notify = "5m"                      # lead-time notification before start
tags   = ["deep-work"]
status = "pending"                 # pending | active | done | skipped | missed | expired

[[block]]
id       = "sync-1"
title    = "Agentic sync-up"
start    = "11:30"
duration = "30m"
notify   = "2m"
run      = ["/home/me/bin/sync.sh", "--fast"]   # argv vector (no shell) — must be allow-listed
status   = "pending"
```

> **Why TOML, not YAML?** TOML is unambiguous to hand-edit (no significant whitespace, no
> "`no` → false" surprises), maps cleanly to a list of blocks, and — unlike the YAML serde crates,
> which are archived/unmaintained or carry a security advisory — it has a maintained, audited Rust
> implementation. Better for agents, better for supply-chain hygiene.

---

## CLI reference

**Authoring**

| Command | Purpose |
|---|---|
| `ccplan set --from <file\|-> [--override-history]` | Replace the whole day's plan. Terminal blocks (done/skipped/missed/expired) are always retained, even if omitted — changing them needs `--override-history`. |
| `ccplan add --title T --start 11:00 [--end\|--duration] [--notify] [--run …] [--id]` | Add or update one block. |
| `ccplan edit <id> [--start …] [--title …] …` | Patch a non-terminal block. |
| `ccplan rm <id>` | Remove a pending block. |
| `ccplan done <id>` / `ccplan skip <id>` | Mark a block complete / skipped. |
| `ccplan clear --yes` | Archive the day and remove its triggers (`--purge` to delete instead of archive). |

**Reading** (all support `--json`)

| Command | Returns |
|---|---|
| `ccplan show` | The full day. |
| `ccplan now` | Array of blocks active right now. |
| `ccplan next` | Array of the next upcoming block(s). |
| `ccplan agenda` | Remaining blocks with countdowns. |

**System**

| Command | Purpose |
|---|---|
| `ccplan apply [--dry-run]` | Reconcile OS triggers to match the plan (idempotent). |
| `ccplan status` | Scheduler health: counts of tracked vs. live OS triggers. |
| `ccplan doctor` | Check the native scheduler + notifier are usable; print fixes. |
| `ccplan completions <shell>` | Print shell completions. |

(`ccplan fire …` exists but is internal — it's what the OS invokes when a block triggers.)

---

## Configuration

`~/.config/ccplan/config.toml` (Linux paths shown):

```toml
grace = "90s"                      # how late a trigger may fire before it's treated as missed

[automation]
enabled = false                    # per-block `run:` is OFF by default
timeout = "5m"                     # max runtime per `run:` command
allowed_executables = [            # `run:`'s argv[0] must be an absolute path on this list
  "/home/me/bin/sync.sh",
]

[notify]
default_lead = "5m"                # notify lead applied to a block that omits its own `notify`
```

---

## How it works

```
 you / an agent          ccplan apply              native OS scheduler         ccplan fire
 ───────────────▶  plan.toml  ───────────▶  systemd / launchd / Task Sched ──────▶  notify + run
   (CLI or edit)   (source of truth)        (one one-shot trigger per event)        + lifecycle
```

1. **Store** — the per-day TOML plan is the single source of truth.
2. **Compile** — `apply` reconciles the plan into native one-shot OS triggers, one per event
   (`notify` / `start` / `end`). Each trigger embeds a **schedule `rev`** (a hash of the block's
   timing) so that re-planned or deleted triggers become inert.
3. **Fire** — when a trigger fires, the OS runs `ccplan fire …`, which checks the trigger is still
   current and on-time (a durable ledger guarantees it acts **at most once**), then notifies, runs
   the command if any, and advances the block's status.

There is **no background daemon** — `ccplan` leans on each OS's own scheduler (systemd user timers,
launchd agents, Windows Task Scheduler), so alerts fire even when `ccplan` isn't running.

---

## Security model

`run:` lets a block execute a command, so the plan file is a **trust boundary**. `ccplan` defends it:

- Automation is **off by default**; you opt in via config.
- A command's executable must be an **absolute path on an allowlist** — nothing else runs.
- Commands run as an **argv vector with no shell** (no `sh -c`), so there's no shell-injection surface.
- The plan file must be **owned by you and not world-writable**, or `ccplan` refuses to run its commands.
- Every fire is **logged**, runs **at most once**, and is **time-bounded**.

See [`SECURITY.md`](SECURITY.md) for the threat model and how to report issues.

---

## Platform support

| | Linux | macOS | Windows |
|---|:---:|:---:|:---:|
| Scheduling | systemd `--user` timers | launchd LaunchAgents | Task Scheduler |
| Notifications | libnotify / D-Bus | NSUserNotification | WinRT toast |
| Notification action buttons | planned | — | — |

A graphical login session is required for notifications (the usual desktop case). Headless use still
schedules and runs automation; `doctor` tells you what's available.

---

## Build from source

Requires a recent stable Rust toolchain (edition 2024; see `rust-toolchain.toml`).

```sh
git clone https://github.com/ankitkpandey1/cc-planner
cd cc-planner
cargo build --release
./target/release/ccplan --help
```

---

## Contributing

Contributions welcome! See [`CONTRIBUTING.md`](CONTRIBUTING.md) and the coding standard in
[`CONVENTIONS.md`](CONVENTIONS.md). In short: conventional-commit messages, `cargo fmt` +
`cargo clippy -- -D warnings` clean, no `unsafe`, tests for every change, and the coverage gate stays
at 100%.

## License

Licensed under either of **[Apache License 2.0](LICENSE-APACHE)** or **[MIT license](LICENSE-MIT)**
at your option. Unless you explicitly state otherwise, any contribution you submit shall be
dual-licensed as above, without additional terms.
