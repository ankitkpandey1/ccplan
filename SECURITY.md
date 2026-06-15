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

## Automation Policy

Plan files never execute through a shell. `run:` is an argv array, and the executable must be allowed
by `automation.allowed_executables` unless automation is disabled. Reports that bypass the
allow-list, invoke a shell implicitly, or mutate terminal history without explicit override are
security issues.

## Dependency Policy

`cargo deny check` is part of the release gate. Advisory and unmaintained allowlists should stay
empty unless there is a documented temporary exception with a removal plan.
