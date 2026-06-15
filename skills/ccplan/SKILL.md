---
name: ccplan
description: Use when an agent needs to install, verify, author, inspect, or apply ccplan day plans non-interactively with stable JSON and exit-code contracts.
short-description: Agent recipe for ccplan day planning.
when-to-use: Use when planning a day with ccplan, scripting ccplan, or validating ccplan scheduler setup.
---

# ccplan

Use this skill to drive `ccplan` as an agent: install it without prompts, check the scheduler, write
a whole day from stdin, apply native triggers, then inspect the result with JSON reads.

<!-- ccplan-agent-recipe:start -->
## Canonical Agent Recipe

Install and verify `ccplan` non-interactively:

```sh
cargo binstall -y ccplan
ccplan --version
ccplan doctor
```

Author the whole day as TOML, then apply it:

```sh
ccplan set --from - <<'TOML'
date = "2099-01-01"
timezone = "Etc/UTC"

[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "45m"
notify = "5m"

[[block]]
id = "review-1"
title = "Review"
start = "10:00"
duration = "30m"
notify = "0m"
TOML

ccplan apply
ccplan show --json
ccplan agenda --json
```

Exit codes:

- `0`: success.
- `2`: usage, validation, or time parsing error.
- `3`: requested plan or block was not found.
- `4`: scheduler backend failure.
- `5`: automation refused by policy or allow-list.
- `6`: terminal history conflict requiring `--override-history`.

JSON contract:

- Reads support `--json`.
- `ccplan show --json` returns the full plan object.
- Query reads such as `ccplan now --json`, `ccplan next --json`, and `ccplan agenda --json` return arrays.
- Empty query results are `[]`, not an error.
<!-- ccplan-agent-recipe:end -->

## Smoke-Test Plan

<!-- ccplan-test-plan:start -->
date = "2099-01-01"
timezone = "Etc/UTC"

[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "45m"
notify = "5m"

[[block]]
id = "review-1"
title = "Review"
start = "10:00"
duration = "30m"
notify = "0m"
<!-- ccplan-test-plan:end -->

## Smoke-Test Commands

<!-- ccplan-test-commands:start -->
ccplan --version
ccplan doctor
ccplan set --from -
ccplan apply
ccplan show --json
ccplan agenda --json
<!-- ccplan-test-commands:end -->
