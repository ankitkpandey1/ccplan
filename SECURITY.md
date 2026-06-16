# Security Policy

Report a private vulnerability by emailing itsankitkp@gmail.com. Please do not open a public issue
for exploitable behavior until a fix or mitigation is available.

## Supported Versions

Security fixes target the latest released `1.x` line.

## Scope

ccplan is a local CLI that writes plan files, schedules native OS triggers, sends desktop
notifications, and can run per-block commands. The most sensitive surfaces are:

- `run:` command execution from plan files.
- `allowed_executables` policy in `config.toml`.
- Scheduler trigger generation and at-most-once fire behavior.
- Atomic plan, history, trigger, and fire-ledger writes.
- MCP server tool surface (`ccplan mcp`).

## Automation Policy

Plan files never execute through a shell. `run:` is an argv array, and the executable must be allowed
by `automation.allowed_executables` unless automation is disabled. Reports that bypass the
allow-list, invoke a shell implicitly, or mutate terminal history without explicit override are
security issues.

## MCP Security Model

The `ccplan mcp` server exposes 16 authoring and read tools. The following invariants hold:

- `fire`, `mcp`, and `completions` are never exposed as MCP tools.
- No MCP tool sets `automation.enabled` or modifies the allowlist.
- No MCP tool calls `authorize_run`; automation is enforced at `fire` time only. `ccplan_snooze_block`
  only shifts a block's time and re-applies (same surface as authoring + apply); it cannot arm or run
  a `run:` command.
- `ccplan_fire_log` is read-only: it returns the existing fire ledger and cannot fire, schedule, or
  mutate anything. Reading history is never an automation path.
- When a `run:` command is stored via MCP but would not execute (automation disabled or executable
  not in the allowlist), the tool response includes a `WARNING` line so the caller knows up front.

## Dependency Policy

`cargo deny check` is part of the release gate. Advisory and unmaintained allowlists should stay
empty unless there is a documented temporary exception with a removal plan.
