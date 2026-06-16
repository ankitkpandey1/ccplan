# ccplan Agent Guide

Use this guide when an agent needs to install, verify, author, inspect, or apply a ccplan day plan.
The canonical recipe below is mirrored in `skills/ccplan/SKILL.md`.

<!-- ccplan-agent-recipe:start -->
## Canonical Agent Recipe

Install and verify `ccplan` non-interactively:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/ankitkpandey1/ccplan/releases/latest/download/ccplan-installer.sh | sh
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

For a single one-shot reminder, skip the TOML — `ccplan remind "Stretch" --in 30m` adds a zero-lead block at now+duration and applies it in one step.

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

## MCP Server

`ccplan mcp` runs a JSON-RPC 2.0 MCP server over stdio. Wire it up in your host's config:

```json
{
  "mcpServers": {
    "ccplan": { "command": "ccplan", "args": ["mcp"] }
  }
}
```

The server exposes 16 tools: `ccplan_plan_day`, `ccplan_apply`, `ccplan_show_plan`,
`ccplan_list_now`, `ccplan_list_next`, `ccplan_show_agenda`, `ccplan_add_block`,
`ccplan_add_reminder`, `ccplan_mark_block`, `ccplan_edit_block`, `ccplan_remove_block`,
`ccplan_snooze_block`, `ccplan_save_template`, `ccplan_list_templates`, `ccplan_apply_template`,
`ccplan_fire_log`.

`ccplan_fire_log` closes the loop: it returns the fire ledger (what notified/activated/missed/
closed, optionally `since` a given instant) so the agent can see what the scheduler did while it was
away and re-plan from there. Read-only — it never fires anything.

`fire`, `mcp`, and `completions` are never exposed as MCP tools.
