//! Hand-rolled synchronous JSON-RPC 2.0 MCP server over stdio.
//!
//! Protocol: newline-delimited JSON-RPC 2.0 (one UTF-8 JSON object per line).
//! Transport: stdio. The real stdin/stdout handles are injected by the single
//! coverage-off wrapper [`run_mcp_server`]; the core [`serve`] uses dynamic
//! dispatch so there is exactly one instantiation and tests drive it with
//! in-memory buffers.

use std::io::{BufRead, Write};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use std::path::PathBuf;

use crate::{
    cli::{
        AddArgs, AgendaArgs, ApplyArgs, BlockTarget, Commands, EditArgs, LogArgs, ReadArgs,
        RemindArgs, SnoozeArgs, TemplateArgs, TemplateCommand, TemplateNameArgs,
    },
    commands::{self, set_from_str, slug_block_id},
    config::{AutomationConfig, Config},
    context::ContextRefs,
    error::{Error, Result},
    lifecycle::{EndBehavior, LifecyclePolicy},
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName,
    },
    store::StoreError,
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Real-handle entry point; grabs stdin/stdout and calls [`serve`].
/// This is the only function in this module with `coverage(off)`.
///
/// # Errors
///
/// Returns any I/O error from reading stdin or writing stdout.
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn run_mcp_server(context: &ContextRefs<'_>) -> Result<()> {
    use std::io::BufReader;
    serve(
        &mut BufReader::new(std::io::stdin().lock()),
        &mut std::io::stdout(),
        context,
    )
}

/// Synchronous newline-delimited JSON-RPC 2.0 MCP server loop.
///
/// Reads one JSON object per line from `reader`, dispatches each request,
/// and writes responses to `writer`. Returns on EOF; propagates I/O errors.
///
/// # Errors
///
/// Returns an error if an I/O read or write operation fails.
pub fn serve(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // clean EOF
        }
        if line.len() > MAX_LINE_BYTES {
            let resp = error_response(&Value::Null, -32700, "request line too large");
            write_msg(writer, &resp)?;
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(trimmed, context) {
            write_msg(writer, &resp)?;
        }
    }
    Ok(())
}

// ── Protocol helpers ──────────────────────────────────────────────────────────

fn write_msg(writer: &mut dyn Write, msg: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Returns `Some(response)` for requests; `None` for notifications (no `id`).
fn handle_line(line: &str, context: &ContextRefs<'_>) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ));
        }
    };

    // JSON-RPC 2.0: absence of "id" (or null id) = notification — do not reply.
    let id = match msg.get("id") {
        None | Some(Value::Null) => return None,
        Some(id) => id,
    };

    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            -32600,
            "invalid request: missing method",
        ));
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    Some(match dispatch_method(method, &params, context) {
        Ok(value) => success_response(id, &value),
        Err(McpErr::MethodNotFound(msg)) => error_response(id, -32601, &msg),
        Err(McpErr::InvalidParams(msg)) => error_response(id, -32602, &msg),
        Err(McpErr::Internal(e)) => error_response(id, -32603, &e.to_string()),
    })
}

fn success_response(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

// ── Method dispatch ───────────────────────────────────────────────────────────

enum McpErr {
    MethodNotFound(String),
    InvalidParams(String),
    Internal(Error),
}

fn dispatch_method(
    method: &str,
    params: &Value,
    context: &ContextRefs<'_>,
) -> std::result::Result<Value, McpErr> {
    match method {
        "initialize" => Ok(handle_initialize()),
        "tools/list" => Ok(handle_tools_list()),
        "tools/call" => handle_tools_call(params, context),
        "ping" => Ok(json!({})),
        _ => Err(McpErr::MethodNotFound(format!(
            "method not found: {method}"
        ))),
    }
}

fn handle_initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {
            "name": "ccplan",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn handle_tools_list() -> Value {
    json!({"tools": tool_catalog()})
}

fn handle_tools_call(
    params: &Value,
    context: &ContextRefs<'_>,
) -> std::result::Result<Value, McpErr> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpErr::InvalidParams("missing tool name".to_owned()))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    // Reload config per call so allowlist/automation edits take effect without a restart.
    let fresh_config =
        Config::load(context.store).map_err(|e| McpErr::Internal(Error::Usage(e.to_string())))?;
    let grace = SignedDuration::from_secs(i64::from(fresh_config.grace.as_seconds()));
    let fresh_policy = LifecyclePolicy::new(grace, EndBehavior::Expire);
    let refreshed = ContextRefs {
        store: context.store,
        clock: context.clock,
        scheduler: context.scheduler,
        notifier: context.notifier,
        policy: fresh_policy,
        config: &fresh_config,
    };

    Ok(call_tool(name, &args, &refreshed))
}

// ── Tool execution ────────────────────────────────────────────────────────────

fn call_tool(name: &str, args: &Value, context: &ContextRefs<'_>) -> Value {
    match name {
        "ccplan_plan_day" => invoke_plan_day(args, context),
        "ccplan_apply" => invoke_apply(args, context),
        "ccplan_show_plan" => invoke_show_plan(args, context),
        "ccplan_list_now" => invoke_list_now(args, context),
        "ccplan_list_next" => invoke_list_next(args, context),
        "ccplan_show_agenda" => invoke_show_agenda(args, context),
        "ccplan_add_block" => invoke_add_block(args, context),
        "ccplan_add_reminder" => invoke_add_reminder(args, context),
        "ccplan_mark_block" => invoke_mark_block(args, context),
        "ccplan_edit_block" => invoke_edit_block(args, context),
        "ccplan_remove_block" => invoke_remove_block(args, context),
        "ccplan_snooze_block" => invoke_snooze_block(args, context),
        "ccplan_save_template" => invoke_save_template(args, context),
        "ccplan_list_templates" => invoke_list_templates(args, context),
        "ccplan_apply_template" => invoke_apply_template(args, context),
        "ccplan_fire_log" => invoke_fire_log(args, context),
        _ => tool_error(&json!({
            "error": "unknown_tool",
            "message": format!("unknown tool: {name}"),
            "hint": "call tools/list to see available tools"
        })),
    }
}

fn invoke_plan_day(args: &Value, context: &ContextRefs<'_>) -> Value {
    match plan_day_inner(args, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

fn plan_day_inner(args: &Value, context: &ContextRefs<'_>) -> Result<String> {
    let override_history = args
        .get("override_history")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let date: Option<PlanDate> = args
        .get("date")
        .and_then(Value::as_str)
        .map(str::parse::<PlanDate>)
        .transpose()
        .map_err(Error::from)?;

    let timezone: TimeZoneName = args
        .get("timezone")
        .and_then(Value::as_str)
        .map(str::parse::<TimeZoneName>)
        .transpose()
        .map_err(Error::from)?
        .unwrap_or_else(|| timezone_from_context(context));

    let blocks_json = args
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Usage("plan_day requires a 'blocks' array".to_owned()))?;

    let mut blocks = Vec::new();
    for (i, bv) in blocks_json.iter().enumerate() {
        blocks.push(parse_mcp_block(bv, i, context.config.notify.default_lead)?);
    }

    // Collect run argvs before blocks is moved into Plan.
    let run_argvs: Vec<Vec<String>> = blocks
        .iter()
        .filter_map(|b| b.run.as_ref().map(|r| r.as_slice().to_vec()))
        .collect();

    let plan_date = date.unwrap_or_else(|| PlanDate::from_jiff_date(context.clock.now().date()));
    let plan = Plan {
        date: plan_date,
        timezone,
        blocks,
    };
    let toml = plan.to_toml().map_err(Error::from)?;

    let mut out = Vec::new();
    set_from_str(&toml, None, override_history, &mut out, context)?;
    let runs: Vec<&[String]> = run_argvs.iter().map(Vec::as_slice).collect();
    let mut result = String::from_utf8_lossy(&out).into_owned();
    if let Some(warn) = run_authorization_warning(&context.config.automation, &runs) {
        result.push_str(&warn);
        result.push('\n');
    }
    Ok(result)
}

fn invoke_apply(args: &Value, context: &ContextRefs<'_>) -> Value {
    let date = extract_date(args);
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cmd = Commands::Apply(ApplyArgs { date, dry_run });
    let mut out = Vec::new();
    match commands::dispatch(Some(cmd), &mut out, context) {
        Ok(()) => tool_ok(&String::from_utf8_lossy(&out)),
        Err(e) => tool_error_from_err(&e),
    }
}

fn invoke_show_plan(args: &Value, context: &ContextRefs<'_>) -> Value {
    let date = extract_date(args);
    let cmd = Commands::Show(ReadArgs { date, json: true });
    let mut out = Vec::new();
    match commands::dispatch(Some(cmd), &mut out, context) {
        Ok(()) => tool_ok(&String::from_utf8_lossy(&out)),
        Err(e) => tool_error_from_err(&e),
    }
}

fn invoke_list_now(args: &Value, context: &ContextRefs<'_>) -> Value {
    let cmd = Commands::Now(ReadArgs {
        date: extract_date(args),
        json: true,
    });
    invoke_read_cmd(cmd, context)
}

fn invoke_list_next(args: &Value, context: &ContextRefs<'_>) -> Value {
    let cmd = Commands::Next(ReadArgs {
        date: extract_date(args),
        json: true,
    });
    invoke_read_cmd(cmd, context)
}

fn invoke_show_agenda(args: &Value, context: &ContextRefs<'_>) -> Value {
    let cmd = Commands::Agenda(AgendaArgs {
        date: extract_date(args),
        json: true,
    });
    invoke_read_cmd(cmd, context)
}

fn invoke_save_template(args: &Value, context: &ContextRefs<'_>) -> Value {
    match template_name_cmd(args, "save_template", TemplateCommand::Save, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

fn invoke_apply_template(args: &Value, context: &ContextRefs<'_>) -> Value {
    match template_name_cmd(args, "apply_template", TemplateCommand::Apply, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

fn invoke_list_templates(_args: &Value, context: &ContextRefs<'_>) -> Value {
    let cmd = Commands::Template(TemplateArgs {
        command: TemplateCommand::List,
    });
    invoke_read_cmd(cmd, context)
}

/// Shared body for the `save`/`apply` template tools: both take `name` (+ optional `date`) and wrap
/// it in the matching `TemplateCommand` variant.
fn template_name_cmd(
    args: &Value,
    tool: &str,
    variant: fn(TemplateNameArgs) -> TemplateCommand,
    context: &ContextRefs<'_>,
) -> Result<String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage(format!("{tool} requires 'name'")))?
        .to_owned();
    let cmd = Commands::Template(TemplateArgs {
        command: variant(TemplateNameArgs {
            name,
            date: extract_date(args),
        }),
    });
    let mut out = Vec::new();
    commands::dispatch(Some(cmd), &mut out, context)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Read-only close-the-loop tool: returns the fire ledger so the agent can see what the scheduler
/// actually did and re-plan. Optional `date` / `since` (RFC 3339) filters narrow the result.
fn invoke_fire_log(args: &Value, context: &ContextRefs<'_>) -> Value {
    let since = match args.get("since").and_then(Value::as_str) {
        Some(raw) => match raw.parse::<Timestamp>() {
            Ok(ts) => Some(ts),
            Err(_) => {
                return tool_error(&json!({
                    "error": "invalid_argument",
                    "message": format!("`since` is not a valid RFC 3339 timestamp: {raw}"),
                    "hint": "pass an RFC 3339 instant like 2026-06-16T09:00:00Z"
                }));
            }
        },
        None => None,
    };
    let cmd = Commands::Log(LogArgs {
        date: extract_date(args),
        since,
        json: true,
    });
    invoke_read_cmd(cmd, context)
}

/// Shared dispatch for the three read list commands; maps `NotFound` to an empty JSON array.
fn invoke_read_cmd(cmd: Commands, context: &ContextRefs<'_>) -> Value {
    let mut out = Vec::new();
    match commands::dispatch(Some(cmd), &mut out, context) {
        Ok(()) => tool_ok(&String::from_utf8_lossy(&out)),
        Err(Error::NotFound(_)) => tool_ok("[]\n"),
        Err(e) => tool_error_from_err(&e),
    }
}

fn invoke_add_block(args: &Value, context: &ContextRefs<'_>) -> Value {
    match add_block_inner(args, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

#[allow(clippy::too_many_lines)]
fn add_block_inner(args: &Value, context: &ContextRefs<'_>) -> Result<String> {
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("add_block requires 'title'".to_owned()))?
        .to_owned();
    let start: ClockTime = args
        .get("start")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("add_block requires 'start'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let end: Option<ClockTime> = args
        .get("end")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let duration: Option<DurationSpec> = args
        .get("duration")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    match (&end, &duration) {
        (Some(_), Some(_)) => {
            return Err(Error::Usage(
                "add_block: set exactly one of 'end' or 'duration'".to_owned(),
            ));
        }
        (None, None) => {
            return Err(Error::Usage(
                "add_block: set 'end' or 'duration'".to_owned(),
            ));
        }
        _ => {}
    }
    let notify: Option<Lead> = args
        .get("notify")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let id: Option<BlockId> = args
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let tags: Vec<String> = args
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let run: Vec<String> = args
        .get("run")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let date = extract_date(args);
    let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
    let run_copy = run.clone();
    let add_args = AddArgs {
        date: date.clone(),
        id,
        title,
        start,
        end,
        duration,
        notify,
        tags,
        run,
    };
    let mut out = Vec::new();
    commands::dispatch(Some(Commands::Add(add_args)), &mut out, context)?;
    if apply {
        let apply_cmd = Some(Commands::Apply(ApplyArgs {
            date,
            dry_run: false,
        }));
        commands::dispatch(apply_cmd, &mut out, context)?;
    }
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if text.is_empty() {
        "block added".clone_into(&mut text);
    }
    let run_refs: Vec<&[String]> = if run_copy.is_empty() {
        vec![]
    } else {
        vec![&run_copy]
    };
    if let Some(warn) = run_authorization_warning(&context.config.automation, &run_refs) {
        text.push_str(&warn);
        text.push('\n');
    }
    Ok(text)
}

fn invoke_add_reminder(args: &Value, context: &ContextRefs<'_>) -> Value {
    match add_reminder_inner(args, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

fn add_reminder_inner(args: &Value, context: &ContextRefs<'_>) -> Result<String> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("add_reminder requires 'text'".to_owned()))?
        .to_owned();
    let fire_in: DurationSpec = args
        .get("in_duration")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("add_reminder requires 'in_duration'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let id: Option<BlockId> = args
        .get("id")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let mut out = Vec::new();
    let remind_cmd = Some(Commands::Remind(RemindArgs { text, fire_in, id }));
    commands::dispatch(remind_cmd, &mut out, context)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn invoke_mark_block(args: &Value, context: &ContextRefs<'_>) -> Value {
    match mark_block_inner(args, context) {
        Ok(()) => tool_ok("block marked"),
        Err(e) => tool_error_from_err(&e),
    }
}

fn mark_block_inner(args: &Value, context: &ContextRefs<'_>) -> Result<()> {
    let id: BlockId = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("mark_block requires 'id'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let status_str = args
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("mark_block requires 'status'".to_owned()))?;
    let cmd = match status_str {
        "done" => Commands::Done(BlockTarget { id }),
        "skipped" => Commands::Skip(BlockTarget { id }),
        other => {
            return Err(Error::Usage(format!(
                "mark_block: status must be 'done' or 'skipped', got '{other}'"
            )));
        }
    };
    let mut out = Vec::new();
    commands::dispatch(Some(cmd), &mut out, context)
}

fn invoke_edit_block(args: &Value, context: &ContextRefs<'_>) -> Value {
    match edit_block_inner(args, context) {
        Ok(()) => tool_ok("block updated"),
        Err(e) => tool_error_from_err(&e),
    }
}

fn edit_block_inner(args: &Value, context: &ContextRefs<'_>) -> Result<()> {
    let id: BlockId = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("edit_block requires 'id'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let title: Option<String> = args.get("title").and_then(Value::as_str).map(str::to_owned);
    let start: Option<ClockTime> = args
        .get("start")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let end: Option<ClockTime> = args
        .get("end")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let duration: Option<DurationSpec> = args
        .get("duration")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let notify: Option<Lead> = args
        .get("notify")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let run: Vec<String> = args
        .get("run")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let edit_args = EditArgs {
        id,
        date: extract_date(args),
        title,
        start,
        end,
        duration,
        notify,
        run,
    };
    let mut out = Vec::new();
    commands::dispatch(Some(Commands::Edit(edit_args)), &mut out, context)
}

fn invoke_remove_block(args: &Value, context: &ContextRefs<'_>) -> Value {
    match remove_block_inner(args, context) {
        Ok(()) => tool_ok("block removed"),
        Err(e) => tool_error_from_err(&e),
    }
}

/// Close-the-loop write tool: push a block later by a duration and re-apply in one call.
fn invoke_snooze_block(args: &Value, context: &ContextRefs<'_>) -> Value {
    match snooze_block_inner(args, context) {
        Ok(text) => tool_ok(&text),
        Err(e) => tool_error_from_err(&e),
    }
}

fn snooze_block_inner(args: &Value, context: &ContextRefs<'_>) -> Result<String> {
    let id: BlockId = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("snooze_block requires 'id'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let by: DurationSpec = args
        .get("by")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("snooze_block requires 'by'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let cmd = Commands::Snooze(SnoozeArgs {
        id,
        by,
        date: extract_date(args),
    });
    let mut out = Vec::new();
    commands::dispatch(Some(cmd), &mut out, context)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn remove_block_inner(args: &Value, context: &ContextRefs<'_>) -> Result<()> {
    let id: BlockId = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage("remove_block requires 'id'".to_owned()))?
        .parse()
        .map_err(Error::from)?;
    let mut out = Vec::new();
    commands::dispatch(Some(Commands::Rm(BlockTarget { id })), &mut out, context)
}

// ── Tool result helpers ───────────────────────────────────────────────────────

fn tool_ok(text: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    })
}

fn tool_error(payload: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": payload.to_string()}],
        "isError": true
    })
}

fn tool_error_from_err(e: &Error) -> Value {
    tool_error(&json!({
        "error": error_code(e),
        "exit_code": e.exit_code(),
        "message": e.to_string(),
        "hint": error_hint(e)
    }))
}

fn is_history_conflict(e: &Error) -> bool {
    matches!(
        e,
        Error::HistoryConflict { .. } | Error::Store(StoreError::TerminalHistory { .. })
    )
}

fn error_code(e: &Error) -> &'static str {
    if is_history_conflict(e) {
        return "history_conflict";
    }
    match e {
        Error::NotFound(_) => "not_found",
        Error::AutomationRefused(_) => "automation_refused",
        Error::Scheduler(_) => "scheduler_error",
        Error::Plan(_) => "plan_error",
        Error::Store(StoreError::Locked) => "store_locked",
        _ => "internal_error",
    }
}

fn error_hint(e: &Error) -> &'static str {
    if is_history_conflict(e) {
        return "pass override_history: true to replace terminal blocks";
    }
    match e {
        Error::NotFound(_) => "use ccplan_plan_day to create a plan first",
        Error::AutomationRefused(_) => "enable automation in config and allowlist the executable",
        Error::Scheduler(_) => "run ccplan doctor to diagnose the scheduler",
        Error::Store(StoreError::Locked) => {
            "another writer holds the store lock; retry in a moment"
        }
        _ => "check the error message for details",
    }
}

/// Returns a warning string if any argv in `runs` would fail the automation check.
///
/// `runs` is a flat list of run argv vectors (one per block that carries a `run:` command).
/// An empty slice means no run commands → returns `None` immediately.
fn run_authorization_warning(automation: &AutomationConfig, runs: &[&[String]]) -> Option<String> {
    if runs.is_empty() {
        return None;
    }
    if !automation.enabled {
        return Some(
            "WARNING: one or more blocks have run: commands, but automation is disabled; \
             the commands will NOT execute at fire time (the notification will still fire). \
             Enable automation in config to arm them."
                .to_owned(),
        );
    }
    let unlisted: Vec<&str> = runs
        .iter()
        .filter_map(|argv| argv.first().map(String::as_str))
        .filter(|p| !automation.allowed_executables.contains(&PathBuf::from(p)))
        .collect();
    if unlisted.is_empty() {
        return None;
    }
    Some(format!(
        "WARNING: the following run: executables are not in the allowlist and will NOT \
         execute at fire time (the notification will still fire): {}. \
         Add them to allowed_executables in config to arm them.",
        unlisted.join(", ")
    ))
}

// ── Field helpers ─────────────────────────────────────────────────────────────

fn extract_date(args: &Value) -> Option<PlanDate> {
    args.get("date")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

fn timezone_from_context(context: &ContextRefs<'_>) -> TimeZoneName {
    let now = context.clock.now();
    now.time_zone()
        .iana_name()
        .unwrap_or("Etc/UTC")
        .parse()
        .expect("IANA timezone names are always valid TimeZoneNames")
}

fn parse_mcp_block(val: &Value, index: usize, default_lead: Lead) -> Result<Block> {
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage(format!("block[{index}] missing required field 'title'")))?
        .to_owned();

    let start: ClockTime = val
        .get("start")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage(format!("block[{index}] missing required field 'start'")))?
        .parse()
        .map_err(Error::from)?;

    let end: Option<ClockTime> = val
        .get("end")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;
    let duration: Option<DurationSpec> = val
        .get("duration")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?;

    let span = match (end, duration) {
        (Some(e), None) => Span::End(e),
        (None, Some(d)) => Span::Duration(d),
        (Some(_), Some(_)) => {
            return Err(Error::Usage(format!(
                "block[{index}] must set exactly one of 'end' or 'duration'"
            )));
        }
        (None, None) => {
            return Err(Error::Usage(format!(
                "block[{index}] must set 'end' or 'duration'"
            )));
        }
    };

    let notify: Lead = val
        .get("notify")
        .and_then(Value::as_str)
        .map(|s| s.parse().map_err(Error::from))
        .transpose()?
        .unwrap_or(default_lead);

    let id: BlockId = match val.get("id").and_then(Value::as_str) {
        Some(s) => s.parse().map_err(Error::from)?,
        None => slug_block_id(&title)?,
    };

    let tags: Vec<String> = val
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let run: Option<Run> = match val.get("run").and_then(Value::as_array) {
        None => None,
        Some(arr) => {
            let argv: Vec<String> = arr
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if argv.is_empty() {
                None
            } else {
                Some(Run::new(argv).map_err(Error::from)?)
            }
        }
    };

    Ok(Block {
        id,
        title,
        start,
        span,
        notify,
        tags,
        status: Status::Pending,
        run,
    })
}

// ── Tool catalog ──────────────────────────────────────────────────────────────

fn tool_catalog() -> Vec<Value> {
    vec![
        plan_day_schema(),
        apply_schema(),
        show_plan_schema(),
        list_now_schema(),
        list_next_schema(),
        show_agenda_schema(),
        add_block_schema(),
        add_reminder_schema(),
        mark_block_schema(),
        edit_block_schema(),
        remove_block_schema(),
        snooze_block_schema(),
        save_template_schema(),
        list_templates_schema(),
        apply_template_schema(),
        fire_log_schema(),
    ]
}

fn plan_day_schema() -> Value {
    json!({
        "name": "ccplan_plan_day",
        "description": "Set the day's plan from structured blocks. Preserves terminal (done/skipped/missed/expired) blocks unless override_history is true. Call ccplan_apply for changes to take effect.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {
                    "type": "string",
                    "description": "ISO date YYYY-MM-DD. Defaults to today."
                },
                "timezone": {
                    "type": "string",
                    "description": "IANA timezone name e.g. 'America/New_York'. Defaults to the server clock timezone."
                },
                "blocks": {
                    "type": "array",
                    "description": "Blocks to plan for the day.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Block ID (alphanumeric/-/_/.). Auto-generated from title if omitted."
                            },
                            "title": {"type": "string", "description": "Block title."},
                            "start": {
                                "type": "string",
                                "description": "Start time HH:MM (24-hour)."
                            },
                            "end": {
                                "type": "string",
                                "description": "End time HH:MM. Set exactly one of end or duration."
                            },
                            "duration": {
                                "type": "string",
                                "description": "Duration e.g. '30m', '1h', '1h30m'. Set exactly one of end or duration."
                            },
                            "notify": {
                                "type": "string",
                                "description": "Notification lead before start e.g. '5m'. '0m' = notify at start. Defaults to config default_lead."
                            },
                            "tags": {
                                "type": "array",
                                "items": {"type": "string"}
                            },
                            "run": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "argv to execute at block start. argv[0] must be an absolute path. Requires automation enabled and argv[0] allowlisted in config."
                            }
                        },
                        "required": ["title", "start"]
                    }
                },
                "override_history": {
                    "type": "boolean",
                    "description": "If true, replaces terminal (done/skipped/missed/expired) blocks. Default false."
                }
            },
            "required": ["blocks"]
        }
    })
}

fn apply_schema() -> Value {
    json!({
        "name": "ccplan_apply",
        "description": "Reconcile the plan against the OS scheduler (systemd/launchd/Task Scheduler), creating or removing native time triggers. Run after any plan mutation to make OS triggers live.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {
                    "type": "string",
                    "description": "ISO date YYYY-MM-DD. Defaults to today."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview changes without applying them."
                }
            }
        }
    })
}

fn fire_log_schema() -> Value {
    json!({
        "name": "ccplan_fire_log",
        "description": "Return the fire ledger — what the scheduler actually did when blocks' events fired (notify/activate/missed/close), newest filters applied. This closes the loop: read it to see what happened while you were away, then re-plan. Each entry has ts, date, id, event, outcome, and a human-readable detail. Returns [] when nothing has fired. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {
                    "type": "string",
                    "description": "ISO date YYYY-MM-DD. Only show fires for this plan date."
                },
                "since": {
                    "type": "string",
                    "description": "RFC 3339 instant (e.g. 2026-06-16T09:00:00Z). Only show fires at or after this time — e.g. what fired since you last looked."
                }
            }
        }
    })
}

fn show_plan_schema() -> Value {
    json!({
        "name": "ccplan_show_plan",
        "description": "Return the day's plan as JSON. Includes all blocks with their statuses. Use to inspect what is planned and check whether blocks need updating.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {
                    "type": "string",
                    "description": "ISO date YYYY-MM-DD. Defaults to today."
                }
            }
        }
    })
}

fn list_now_schema() -> Value {
    json!({
        "name": "ccplan_list_now",
        "description": "Return blocks that are currently active (started but not yet ended). Returns [] when no plan exists or no block is active right now.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."}
            }
        }
    })
}

fn list_next_schema() -> Value {
    json!({
        "name": "ccplan_list_next",
        "description": "Return the next upcoming block(s) — all blocks starting at the same nearest future time. Returns [] when no plan exists or nothing is coming up today.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."}
            }
        }
    })
}

fn show_agenda_schema() -> Value {
    json!({
        "name": "ccplan_show_agenda",
        "description": "Return all non-terminal blocks whose end time has not yet passed, ordered by start time. Returns [] when no plan exists or the day is complete.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."}
            }
        }
    })
}

fn add_block_schema() -> Value {
    json!({
        "name": "ccplan_add_block",
        "description": "Add or replace a single block in the day's plan. Set exactly one of 'end' or 'duration'. Call ccplan_apply to take effect.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Block title."},
                "start": {"type": "string", "description": "Start time HH:MM (24-hour)."},
                "end": {"type": "string", "description": "End time HH:MM. Set exactly one of end or duration."},
                "duration": {"type": "string", "description": "Duration e.g. '30m', '1h30m'. Set exactly one of end or duration."},
                "notify": {"type": "string", "description": "Notification lead e.g. '5m'. Defaults to config default_lead."},
                "id": {"type": "string", "description": "Block ID. Auto-generated from title if omitted."},
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."},
                "tags": {"type": "array", "items": {"type": "string"}},
                "run": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "argv to execute at block start. argv[0] must be absolute and allowlisted."
                },
                "apply": {
                    "type": "boolean",
                    "description": "If true, also run ccplan_apply after adding. Default false."
                }
            },
            "required": ["title", "start"]
        }
    })
}

fn add_reminder_schema() -> Value {
    json!({
        "name": "ccplan_add_reminder",
        "description": "Set a one-shot OS notification to fire in a given duration from now, then apply immediately.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "Reminder text shown in the notification."},
                "in_duration": {
                    "type": "string",
                    "description": "Duration until the reminder fires, e.g. '30m', '1h', '2h30m'."
                },
                "id": {"type": "string", "description": "Block ID. Auto-generated from text if omitted."}
            },
            "required": ["text", "in_duration"]
        }
    })
}

fn mark_block_schema() -> Value {
    json!({
        "name": "ccplan_mark_block",
        "description": "Mark a block done or skipped. Note: missed and expired are system-assigned. Call ccplan_apply to take effect.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Block ID."},
                "status": {
                    "type": "string",
                    "enum": ["done", "skipped"],
                    "description": "New status. Only 'done' and 'skipped' may be set manually."
                }
            },
            "required": ["id", "status"]
        }
    })
}

fn edit_block_schema() -> Value {
    json!({
        "name": "ccplan_edit_block",
        "description": "Edit fields of an existing non-terminal block. Set at most one of 'end' or 'duration'. Call ccplan_apply to take effect.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Block ID to edit."},
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."},
                "title": {"type": "string"},
                "start": {"type": "string", "description": "Start time HH:MM."},
                "end": {"type": "string", "description": "End time HH:MM."},
                "duration": {"type": "string", "description": "Duration e.g. '30m'."},
                "notify": {"type": "string", "description": "Notification lead e.g. '5m'."},
                "run": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["id"]
        }
    })
}

fn remove_block_schema() -> Value {
    json!({
        "name": "ccplan_remove_block",
        "description": "Remove a non-terminal block from today's plan. Call ccplan_apply to take effect.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Block ID to remove."}
            },
            "required": ["id"]
        }
    })
}

fn snooze_block_schema() -> Value {
    json!({
        "name": "ccplan_snooze_block",
        "description": "Push a non-terminal block later by a duration and re-apply in one call — react to a fire (e.g. a notify you couldn't act on) by sliding the block instead of recomputing absolute times. Refused if the slide would cross midnight (no day rollover).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Block ID to snooze."},
                "by": {"type": "string", "description": "How much later to move it, e.g. '10m', '1h', '1h30m'."},
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."}
            },
            "required": ["id", "by"]
        }
    })
}

fn save_template_schema() -> Value {
    json!({
        "name": "ccplan_save_template",
        "description": "Save the plan for a date as a named, reusable day template. Capture a good day shape once, then stamp it onto future dates with ccplan_apply_template. Name must be a slug (letters, digits, '-', '_').",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Template name (letters, digits, '-', '_')."},
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD to capture. Defaults to today."}
            },
            "required": ["name"]
        }
    })
}

fn list_templates_schema() -> Value {
    json!({
        "name": "ccplan_list_templates",
        "description": "List saved day-template names, one per line. Returns 'no templates saved' when none exist. Read-only.",
        "inputSchema": {"type": "object", "properties": {}}
    })
}

fn apply_template_schema() -> Value {
    json!({
        "name": "ccplan_apply_template",
        "description": "Instantiate a saved template onto a date (every block reset to pending) and apply it, arming OS triggers. Like ccplan_plan_day, instantiating over a day with terminal (done/skipped/missed/expired) blocks is refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Template name to instantiate."},
                "date": {"type": "string", "description": "ISO date YYYY-MM-DD. Defaults to today."}
            },
            "required": ["name"]
        }
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use assert_fs::TempDir;
    use jiff::Zoned;
    use serde_json::{Value, json};

    use std::path::PathBuf;

    use crate::{
        config::{AutomationConfig, Config},
        context::{Context, RecordingNotifier, RecordingScheduler},
        error::Error,
        model::{
            Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Span, Status,
            TimeZoneName,
        },
        store::{HistoryPolicy, Store, StoreError},
        time::FixedClock,
    };

    use super::serve;

    fn test_context() -> (
        TempDir,
        Context<FixedClock, RecordingScheduler, RecordingNotifier>,
    ) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let clock = FixedClock::new(
            "2026-06-08T10:00:00+05:30[Asia/Kolkata]"
                .parse::<Zoned>()
                .unwrap(),
        );
        let context = Context::new(
            store,
            clock,
            RecordingScheduler::default(),
            RecordingNotifier::default(),
            Config::default(),
        );
        (temp, context)
    }

    fn run_serve<C, S, N>(context: &Context<C, S, N>, input: &[u8]) -> Vec<Value>
    where
        C: crate::time::Clock,
        S: crate::context::Scheduler,
        N: crate::context::Notifier,
    {
        let mut output = Vec::new();
        serve(
            &mut std::io::BufReader::new(input),
            &mut output,
            &context.as_refs(),
        )
        .expect("serve should not error");
        String::from_utf8(output)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("response must be valid JSON"))
            .collect()
    }

    #[allow(clippy::needless_pass_by_value)]
    fn req(id: i64, method: &str, params: Value) -> String {
        format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
    }

    fn notif(method: &str) -> String {
        format!("{}\n", json!({"jsonrpc": "2.0", "method": method}))
    }

    #[test]
    fn full_handshake_plan_day_apply_show_plan() {
        let (_temp, context) = test_context();

        let mut input = String::new();
        input.push_str(&req(
            1,
            "initialize",
            json!({"protocolVersion": "2024-11-05", "clientInfo": {"name": "test", "version": "1.0"}}),
        ));
        input.push_str(&notif("notifications/initialized"));
        input.push_str(&req(2, "tools/list", json!({})));
        input.push_str(&req(
            3,
            "tools/call",
            json!({
                "name": "ccplan_plan_day",
                "arguments": {
                    "date": "2026-06-08",
                    "timezone": "Asia/Kolkata",
                    "blocks": [
                        {
                            "title": "Focus time",
                            "start": "11:00",
                            "end": "11:30",
                            "tags": ["deep-work"]
                        },
                        {
                            "id": "lunch",
                            "title": "Lunch",
                            "start": "14:00",
                            "duration": "30m"
                        }
                    ]
                }
            }),
        ));
        input.push_str(&req(
            4,
            "tools/call",
            json!({"name": "ccplan_apply", "arguments": {}}),
        ));
        input.push_str(&req(
            5,
            "tools/call",
            json!({"name": "ccplan_show_plan", "arguments": {}}),
        ));

        let responses = run_serve(&context, input.as_bytes());

        // 5 responses (notification is silent)
        assert_eq!(responses.len(), 5, "{responses:?}");

        // initialize
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
        assert!(responses[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ccplan");

        // tools/list
        assert_eq!(responses[1]["id"], 2);
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccplan_plan_day"));
        assert!(names.contains(&"ccplan_apply"));
        assert!(names.contains(&"ccplan_show_plan"));

        // plan_day
        assert_eq!(responses[2]["id"], 3);
        assert_eq!(responses[2]["result"]["isError"], false);
        let plan_text = responses[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(plan_text.contains("stored"));

        // apply
        assert_eq!(responses[3]["id"], 4);
        assert_eq!(responses[3]["result"]["isError"], false);

        // show_plan
        assert_eq!(responses[4]["id"], 5);
        assert_eq!(responses[4]["result"]["isError"], false);
        let show_text = responses[4]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let plan: Value = serde_json::from_str(show_text).expect("show_plan must be JSON");
        assert_eq!(plan["date"], "2026-06-08");
        assert_eq!(plan["block"].as_array().unwrap().len(), 2);

        // plan_day did NOT touch process stdin (the test completes = no stdin hang)
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.blocks.len(), 2);
        assert_eq!(stored.blocks[0].id.as_str(), "focus-time");
        assert_eq!(stored.blocks[1].id.as_str(), "lunch");
    }

    #[test]
    fn protocol_version_negotiation_returns_server_version() {
        let (_temp, context) = test_context();
        let input = req(1, "initialize", json!({"protocolVersion": "2099-01-01"}));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn notifications_get_no_response() {
        let (_temp, context) = test_context();
        let mut input = String::new();
        input.push_str(&notif("notifications/initialized"));
        input.push_str(&notif("notifications/cancelled"));
        input.push_str(&req(1, "ping", json!({})));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses.len(), 1, "only ping gets a response");
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"], json!({}));
    }

    #[test]
    fn unknown_method_returns_error_code_32601() {
        let (_temp, context) = test_context();
        let input = req(1, "no/such/method", json!({}));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["error"]["code"], -32601);
    }

    #[test]
    fn unknown_tool_name_returns_iserror_true() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "not_a_tool", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn malformed_json_line_returns_parse_error() {
        let (_temp, context) = test_context();
        let responses = run_serve(&context, b"this is not json\n");
        assert_eq!(responses[0]["error"]["code"], -32700);
    }

    #[test]
    fn empty_lines_are_ignored() {
        let (_temp, context) = test_context();
        let input = format!("\n\n\n{}", req(1, "ping", json!({})));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 1);
    }

    #[test]
    fn eof_causes_clean_shutdown() {
        let (_temp, context) = test_context();
        let responses = run_serve(&context, b"");
        assert!(responses.is_empty());
    }

    #[test]
    fn tools_call_without_name_returns_invalid_params() {
        let (_temp, context) = test_context();
        let input = req(1, "tools/call", json!({"arguments": {}}));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["error"]["code"], -32602);
    }

    #[test]
    fn request_with_null_id_is_treated_as_notification() {
        let (_temp, context) = test_context();
        // Send a message with explicit null id (treated as notification → no reply)
        // then a real request to confirm the server is still running.
        let null_id_msg = format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"})
        );
        let real_req = req(1, "ping", json!({}));
        let input = format!("{null_id_msg}{real_req}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 1);
    }

    #[test]
    fn missing_method_field_returns_invalid_request() {
        let (_temp, context) = test_context();
        let input = format!("{}\n", json!({"jsonrpc": "2.0", "id": 1}));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["error"]["code"], -32600);
    }

    #[test]
    fn line_too_large_returns_error_and_closes_connection() {
        let (_temp, context) = test_context();
        let oversized = "x".repeat(super::MAX_LINE_BYTES + 1) + "\n";
        let input = format!("{oversized}{}", req(1, "ping", json!({})));
        let mut out = Vec::new();
        serve(
            &mut std::io::BufReader::new(input.as_bytes()),
            &mut out,
            &context.as_refs(),
        )
        .unwrap();
        let responses: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Error for the oversized line; ping is never seen (server closed)
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["error"]["code"], -32700);
    }

    // plan_day error paths

    #[test]
    fn plan_day_missing_blocks_array_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_block_missing_title_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {"blocks": [{"start": "09:00", "end": "10:00"}]}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_block_missing_start_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {"blocks": [{"title": "Meeting", "end": "10:00"}]}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_block_missing_end_and_duration_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {"blocks": [{"title": "Oops", "start": "09:00"}]}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_block_both_end_and_duration_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {"blocks": [
                {"title": "Oops", "start": "09:00", "end": "10:00", "duration": "30m"}
            ]}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_bad_date_format_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "not-a-date",
                "blocks": [{"title": "X", "start": "09:00", "end": "10:00"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_bad_timezone_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "timezone": "Not/A/Zone",
                "blocks": [{"title": "X", "start": "09:00", "end": "10:00"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_uses_clock_timezone_when_none_given() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08",
                "blocks": [{"title": "Standup", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.timezone.as_str(), "Asia/Kolkata");
    }

    #[test]
    fn plan_day_with_explicit_block_id() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"id": "my-block", "title": "X", "start": "09:00", "end": "10:00"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.blocks[0].id.as_str(), "my-block");
    }

    #[test]
    fn plan_day_with_run_field() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"title": "Sync", "start": "09:00", "end": "09:30",
                             "run": ["/usr/bin/echo", "hello"]}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert!(stored.blocks[0].run.is_some());
    }

    #[test]
    fn plan_day_with_empty_run_array_means_no_run() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"title": "Sync", "start": "09:00", "end": "09:30", "run": []}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert!(stored.blocks[0].run.is_none());
    }

    #[test]
    fn show_plan_no_plan_returns_iserror_not_found() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_show_plan", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["error"], "not_found");
        assert_eq!(payload["exit_code"], 3);
        assert_eq!(
            payload["hint"],
            "use ccplan_plan_day to create a plan first"
        );
    }

    #[test]
    fn apply_on_plan_with_history_conflict_returns_iserror() {
        let (_temp, context) = test_context();
        // Store a plan with a done block
        let plan = Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
            blocks: vec![Block {
                id: "done-block".parse::<BlockId>().unwrap(),
                title: "Done".to_owned(),
                start: "09:00".parse::<ClockTime>().unwrap(),
                span: Span::Duration(DurationSpec::from_seconds(1800).unwrap()),
                notify: Lead::from_seconds(0).unwrap(),
                tags: vec![],
                status: Status::Done,
                run: None,
            }],
        };
        context
            .store
            .set_plan(&plan, HistoryPolicy::Preserve)
            .unwrap();

        // plan_day with a block that conflicts with the done block (same id, preserve_history)
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"id": "done-block", "title": "Done", "start": "09:00", "end": "10:00"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["error"], "history_conflict");
        assert_eq!(payload["exit_code"], 6);
        assert_eq!(
            payload["hint"],
            "pass override_history: true to replace terminal blocks"
        );
    }

    #[test]
    fn tools_call_with_corrupt_config_returns_rpc_internal_error() {
        let (_temp, context) = test_context();
        // Write invalid TOML to the config file so Config::load fails inside handle_tools_call.
        let config_path = context.store.config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, b"[[[not valid toml").unwrap();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_show_plan", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        // Config::load failure propagates as McpErr::Internal → JSON-RPC error code -32603.
        assert_eq!(responses[0]["error"]["code"], -32603);
    }

    #[test]
    fn invoke_apply_with_unavailable_scheduler_returns_scheduler_error() {
        use crate::context::UnavailableScheduler;

        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let clock = FixedClock::new(
            "2026-06-08T10:00:00+05:30[Asia/Kolkata]"
                .parse::<Zoned>()
                .unwrap(),
        );
        let context = Context::new(
            store,
            clock,
            UnavailableScheduler,
            RecordingNotifier::default(),
            Config::default(),
        );

        // Plan a block first (plan_day does not use the scheduler).
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08",
                "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Standup", "start": "09:00", "end": "09:30"}]
            }}),
        );
        // Apply will fail: plan exists, scheduler.prepare() returns SchedulerError::Unavailable.
        let apply_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_apply", "arguments": {"date": "2026-06-08"}}),
        );
        let input = format!("{plan_input}{apply_input}");
        let responses = run_serve(&context, input.as_bytes());

        assert_eq!(
            responses[0]["result"]["isError"], false,
            "plan_day should succeed"
        );
        assert_eq!(responses[1]["result"]["isError"], true, "apply should fail");
        let text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["error"], "scheduler_error");
        assert_eq!(
            payload["hint"],
            "run ccplan doctor to diagnose the scheduler"
        );
    }

    #[test]
    fn plan_day_duplicate_block_ids_returns_iserror() {
        // to_toml() calls validate() which rejects duplicate IDs → exercises the to_toml error path.
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [
                    {"id": "dup", "title": "A", "start": "09:00", "end": "10:00"},
                    {"id": "dup", "title": "B", "start": "11:00", "end": "12:00"}
                ]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_block_invalid_start_time_returns_iserror() {
        // parse::<ClockTime>() failure → exercises the start parse error path.
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"title": "X", "start": "25:00", "end": "26:00"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn error_code_and_hint_cover_automation_refused_and_plan_variants() {
        use crate::context::SchedulerError;
        use crate::error::Error;
        use crate::model::Plan;

        let auto = Error::AutomationRefused("disabled".to_owned());
        assert_eq!(super::error_code(&auto), "automation_refused");
        assert_eq!(
            super::error_hint(&auto),
            "enable automation in config and allowlist the executable"
        );

        let plan_err = Error::from(Plan::from_toml("date = 'bad'").unwrap_err());
        assert_eq!(super::error_code(&plan_err), "plan_error");

        let sched = Error::from(SchedulerError::Unavailable);
        assert_eq!(super::error_code(&sched), "scheduler_error");
        assert_eq!(
            super::error_hint(&sched),
            "run ccplan doctor to diagnose the scheduler"
        );
    }

    #[test]
    fn plan_day_with_empty_first_run_arg_returns_iserror() {
        // Run::new(argv) fails when argv[0] is empty → exercises the Run error path.
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"title": "X", "start": "09:00", "end": "10:00", "run": ["", "echo"]}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn plan_day_with_notify_field_parses_lead() {
        // Exercises the notify-field closure path in parse_mcp_block.
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "blocks": [{"title": "X", "start": "09:00", "end": "10:00", "notify": "5m"}]
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
    }

    // ── M3: list_now / list_next / show_agenda ─────────────────────────────────

    #[test]
    fn list_now_returns_empty_array_when_no_plan() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_list_now", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text.trim(), "[]");
    }

    #[test]
    fn list_now_returns_active_block() {
        let (_temp, context) = test_context();
        // Clock is at 10:00; create a block 10:00–11:00 (starts exactly now → active).
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08",
                "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Now Block", "start": "10:00", "end": "11:00"}]
            }}),
        );
        let list_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_list_now", "arguments": {"date": "2026-06-08"}}),
        );
        let input = format!("{plan_input}{list_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let arr: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
        assert_eq!(arr[0]["id"], "now-block");
    }

    #[test]
    fn list_now_with_corrupt_plan_returns_iserror() {
        let (_temp, context) = test_context();
        let date = "2026-06-08".parse::<PlanDate>().unwrap();
        let plan_path = context.store.plan_path(&date);
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, b"[[[not valid toml").unwrap();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_list_now", "arguments": {"date": "2026-06-08"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn list_next_returns_empty_array_when_no_plan() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_list_next", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text.trim(), "[]");
    }

    #[test]
    fn list_next_returns_upcoming_block() {
        let (_temp, context) = test_context();
        // Clock at 10:00; block starts at 11:00 (future).
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08",
                "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Next Block", "start": "11:00", "end": "11:30"}]
            }}),
        );
        let list_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_list_next", "arguments": {"date": "2026-06-08"}}),
        );
        let input = format!("{plan_input}{list_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let arr: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }

    #[test]
    fn show_agenda_returns_empty_array_when_no_plan() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_show_agenda", "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text.trim(), "[]");
    }

    #[test]
    fn show_agenda_returns_upcoming_entries() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08",
                "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Agenda Block", "start": "11:00", "end": "12:00"}]
            }}),
        );
        let agenda_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_show_agenda", "arguments": {"date": "2026-06-08"}}),
        );
        let input = format!("{plan_input}{agenda_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let arr: Value = serde_json::from_str(text.trim()).unwrap();
        assert!(!arr.as_array().unwrap().is_empty());
    }

    // ── M3: add_block ──────────────────────────────────────────────────────────

    #[test]
    fn add_block_creates_block_on_plan() {
        let (_temp, context) = test_context();
        // First create a plan, then add a block.
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"title": "First", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let add_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_add_block", "arguments": {
                "date": "2026-06-08",
                "title": "Added Block", "start": "10:00", "end": "10:30"
            }}),
        );
        let input = format!("{plan_input}{add_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.blocks.len(), 2);
    }

    #[test]
    fn add_block_with_apply_triggers_scheduler() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Seed", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let add_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_add_block", "arguments": {
                "date": "2026-06-08",
                "title": "Applied Block", "start": "11:00", "end": "11:30", "apply": true
            }}),
        );
        let input = format!("{plan_input}{add_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        // apply output is present in the text
        let text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(!text.is_empty());
    }

    #[test]
    fn add_block_missing_title_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_block",
            "arguments": {"start": "09:00", "end": "10:00"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn add_block_missing_start_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_block",
            "arguments": {"title": "X", "end": "10:00"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn add_block_missing_end_and_duration_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_block",
            "arguments": {"title": "X", "start": "09:00"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn add_block_both_end_and_duration_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_block",
            "arguments": {"title": "X", "start": "09:00", "end": "10:00", "duration": "30m"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    // ── M3: add_reminder ───────────────────────────────────────────────────────

    #[test]
    fn add_reminder_sets_reminder_and_applies() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_reminder",
            "arguments": {"text": "Check in", "in_duration": "1h"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("Check in"));
    }

    #[test]
    fn add_reminder_missing_text_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_reminder",
            "arguments": {"in_duration": "1h"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn add_reminder_missing_in_duration_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_reminder",
            "arguments": {"text": "X"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    // ── M3: mark_block ─────────────────────────────────────────────────────────

    #[test]
    fn mark_block_done_succeeds() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "my-task", "title": "Task", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let mark_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_mark_block",
            "arguments": {"id": "my-task", "status": "done"}}),
        );
        let input = format!("{plan_input}{mark_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.blocks[0].status, Status::Done);
    }

    #[test]
    fn mark_block_skipped_succeeds() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "task-b", "title": "Task B", "start": "10:00", "end": "10:30"}]
            }}),
        );
        let mark_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_mark_block",
            "arguments": {"id": "task-b", "status": "skipped"}}),
        );
        let input = format!("{plan_input}{mark_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
    }

    #[test]
    fn mark_block_invalid_status_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_mark_block",
            "arguments": {"id": "x", "status": "missed"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn mark_block_missing_id_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_mark_block",
            "arguments": {"status": "done"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn mark_block_missing_status_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_mark_block",
            "arguments": {"id": "x"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    // ── M3: edit_block ─────────────────────────────────────────────────────────

    #[test]
    fn edit_block_updates_title() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "edit-me", "title": "Old Title", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let edit_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_edit_block",
            "arguments": {"id": "edit-me", "date": "2026-06-08", "title": "New Title"}}),
        );
        let input = format!("{plan_input}{edit_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.blocks[0].title, "New Title");
    }

    #[test]
    fn edit_block_missing_id_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_edit_block",
            "arguments": {"title": "X"}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn edit_block_not_found_returns_iserror() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Seed", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let edit_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_edit_block",
            "arguments": {"id": "no-such-block", "date": "2026-06-08", "title": "X"}}),
        );
        let input = format!("{plan_input}{edit_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], true);
    }

    // ── M3: remove_block ───────────────────────────────────────────────────────

    #[test]
    fn remove_block_deletes_block() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "rm-me", "title": "Remove Me", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let rm_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_remove_block",
            "arguments": {"id": "rm-me"}}),
        );
        let input = format!("{plan_input}{rm_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        assert!(stored.blocks.is_empty());
    }

    #[test]
    fn remove_block_missing_id_returns_iserror() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_remove_block",
            "arguments": {}}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
    }

    #[test]
    fn remove_block_not_found_returns_iserror() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Seed", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let rm_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_remove_block",
            "arguments": {"id": "no-such-block"}}),
        );
        let input = format!("{plan_input}{rm_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], true);
    }

    #[test]
    fn add_block_with_notify_id_tags_run_covers_optional_closures() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"title": "Seed", "start": "09:00", "end": "09:30"}]
            }}),
        );
        let add_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_add_block", "arguments": {
                "date": "2026-06-08",
                "id": "custom-id",
                "title": "Full Block",
                "start": "11:00",
                "end": "12:00",
                "notify": "5m",
                "tags": ["work", "focus"],
                "run": ["/usr/bin/echo", "hello"]
            }}),
        );
        let input = format!("{plan_input}{add_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
        let stored = context
            .store
            .load_plan(&"2026-06-08".parse::<PlanDate>().unwrap())
            .unwrap()
            .unwrap();
        let added = stored
            .blocks
            .iter()
            .find(|b| b.id.as_str() == "custom-id")
            .unwrap();
        assert_eq!(added.tags, vec!["work", "focus"]);
        assert!(added.run.is_some());
    }

    #[test]
    fn edit_block_with_start_end_notify_covers_optional_closures() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "b1", "title": "Block", "start": "09:00", "end": "10:00"}]
            }}),
        );
        let edit_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_edit_block", "arguments": {
                "id": "b1",
                "date": "2026-06-08",
                "start": "09:30",
                "end": "10:30",
                "notify": "3m"
            }}),
        );
        let input = format!("{plan_input}{edit_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
    }

    #[test]
    fn edit_block_with_duration_and_run_covers_optional_closures() {
        let (_temp, context) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_plan_day", "arguments": {
                "date": "2026-06-08", "timezone": "Asia/Kolkata",
                "blocks": [{"id": "b2", "title": "Block2", "start": "11:00", "end": "12:00"}]
            }}),
        );
        let edit_input = req(
            2,
            "tools/call",
            json!({"name": "ccplan_edit_block", "arguments": {
                "id": "b2",
                "date": "2026-06-08",
                "duration": "45m",
                "run": ["/usr/bin/echo", "hi"]
            }}),
        );
        let input = format!("{plan_input}{edit_input}");
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[1]["result"]["isError"], false);
    }

    #[test]
    fn add_reminder_with_explicit_id_covers_id_closure() {
        let (_temp, context) = test_context();
        let input = req(
            1,
            "tools/call",
            json!({"name": "ccplan_add_reminder", "arguments": {
                "text": "Stand up",
                "in_duration": "30m",
                "id": "standup-reminder"
            }}),
        );
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], false);
    }

    #[test]
    fn tools_list_contains_all_sixteen_tools() {
        let (_temp, context) = test_context();
        let input = req(1, "tools/list", json!({}));
        let responses = run_serve(&context, input.as_bytes());
        let tools = responses[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 16);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in &[
            "ccplan_plan_day",
            "ccplan_apply",
            "ccplan_show_plan",
            "ccplan_list_now",
            "ccplan_list_next",
            "ccplan_show_agenda",
            "ccplan_add_block",
            "ccplan_add_reminder",
            "ccplan_mark_block",
            "ccplan_edit_block",
            "ccplan_remove_block",
            "ccplan_snooze_block",
            "ccplan_save_template",
            "ccplan_list_templates",
            "ccplan_apply_template",
            "ccplan_fire_log",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn fire_log_tool_reads_ledger_and_validates_since() {
        let (_temp, context) = test_context();
        // Seed the ledger directly so the read-only tool has something to return.
        let path = context.store.fire_log_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"ts\":\"2026-06-08T05:30:00Z\",\"date\":\"2026-06-08\",\"id\":\"focus\",\"event\":\"start\",\"outcome\":\"activate\",\"detail\":\"activated\"}\n",
        )
        .unwrap();

        let mut input = notif("notifications/initialized");
        input.push_str(&req(
            1,
            "tools/call",
            json!({"name": "ccplan_fire_log", "arguments": {}}),
        ));
        input.push_str(&req(
            2,
            "tools/call",
            json!({"name": "ccplan_fire_log", "arguments": {"since": "2026-06-08T00:00:00Z"}}),
        ));
        input.push_str(&req(
            3,
            "tools/call",
            json!({"name": "ccplan_fire_log", "arguments": {"since": "not-a-timestamp"}}),
        ));
        let responses = run_serve(&context, input.as_bytes());

        // No-filter read returns the record.
        let text0 = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text0.contains("activate"), "{text0}");
        // Valid `since` still returns it.
        let text1 = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text1.contains("activate"), "{text1}");
        // Invalid `since` is a structured error, not a panic.
        assert_eq!(responses[2]["result"]["isError"], true);
        let err = responses[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(err.contains("RFC 3339"), "{err}");
    }

    #[test]
    fn snooze_block_tool_slides_block_and_reports_errors() {
        let (_temp, context) = test_context();
        // Future block (now is 10:00): snoozing keeps it ahead of the clock.
        let plan = Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
            blocks: vec![Block {
                id: "focus".parse::<BlockId>().unwrap(),
                title: "Focus".to_owned(),
                start: "14:00".parse::<ClockTime>().unwrap(),
                span: Span::Duration(DurationSpec::from_seconds(1800).unwrap()),
                notify: Lead::from_seconds(0).unwrap(),
                tags: vec![],
                status: Status::Pending,
                run: None,
            }],
        };
        context
            .store
            .set_plan(&plan, HistoryPolicy::Preserve)
            .unwrap();

        let mut input = notif("notifications/initialized");
        input.push_str(&req(
            1,
            "tools/call",
            json!({"name": "ccplan_snooze_block", "arguments": {"id": "focus", "by": "1h"}}),
        ));
        input.push_str(&req(
            2,
            "tools/call",
            json!({"name": "ccplan_show_plan", "arguments": {}}),
        ));
        input.push_str(&req(
            3,
            "tools/call",
            json!({"name": "ccplan_snooze_block", "arguments": {"id": "focus"}}),
        ));
        input.push_str(&req(
            4,
            "tools/call",
            json!({"name": "ccplan_snooze_block", "arguments": {"by": "5m"}}),
        ));
        let responses = run_serve(&context, input.as_bytes());

        assert_eq!(responses[0]["result"]["isError"], false);
        let shown = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(shown.contains("15:00"), "{shown}");
        // Missing required `by` is a structured error, not a panic.
        assert_eq!(responses[2]["result"]["isError"], true);
        let err = responses[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(err.contains("snooze_block requires 'by'"), "{err}");
        // Missing required `id` is likewise a structured error.
        assert_eq!(responses[3]["result"]["isError"], true);
        let err = responses[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(err.contains("snooze_block requires 'id'"), "{err}");
    }

    #[test]
    fn template_tools_save_list_and_instantiate() {
        let (_temp, context) = test_context();
        let plan = Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
            blocks: vec![Block {
                id: "focus".parse::<BlockId>().unwrap(),
                title: "Focus".to_owned(),
                start: "14:00".parse::<ClockTime>().unwrap(),
                span: Span::Duration(DurationSpec::from_seconds(1800).unwrap()),
                notify: Lead::from_seconds(0).unwrap(),
                tags: vec![],
                status: Status::Pending,
                run: None,
            }],
        };
        context
            .store
            .set_plan(&plan, HistoryPolicy::Preserve)
            .unwrap();

        let mut input = notif("notifications/initialized");
        input.push_str(&req(
            1,
            "tools/call",
            json!({"name": "ccplan_save_template", "arguments": {"name": "weekday"}}),
        ));
        input.push_str(&req(
            2,
            "tools/call",
            json!({"name": "ccplan_list_templates", "arguments": {}}),
        ));
        input.push_str(&req(
            3,
            "tools/call",
            json!({"name": "ccplan_apply_template", "arguments": {"name": "weekday", "date": "2026-06-09"}}),
        ));
        input.push_str(&req(
            4,
            "tools/call",
            json!({"name": "ccplan_show_plan", "arguments": {"date": "2026-06-09"}}),
        ));
        input.push_str(&req(
            5,
            "tools/call",
            json!({"name": "ccplan_save_template", "arguments": {}}),
        ));
        input.push_str(&req(
            6,
            "tools/call",
            json!({"name": "ccplan_apply_template", "arguments": {"name": "ghost"}}),
        ));
        let responses = run_serve(&context, input.as_bytes());

        assert_eq!(responses[0]["result"]["isError"], false);
        let listed = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(listed.contains("weekday"), "{listed}");
        assert_eq!(responses[2]["result"]["isError"], false);
        // The template was instantiated onto the new date.
        let shown = responses[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(shown.contains("\"focus\""), "{shown}");
        assert!(shown.contains("2026-06-09"), "{shown}");
        // Missing required `name` is a structured error.
        assert_eq!(responses[4]["result"]["isError"], true);
        let err = responses[4]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(err.contains("save_template requires 'name'"), "{err}");
        // Applying a template that does not exist is a structured not_found error.
        assert_eq!(responses[5]["result"]["isError"], true);
        let missing = responses[5]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(missing.contains("not_found"), "{missing}");
    }

    // --- Security / M4 tests ---

    #[test]
    fn tool_catalog_excludes_fire_mcp_completions() {
        let (_temp, context) = test_context();
        let input = req(1, "tools/list", json!({}));
        let responses = run_serve(&context, input.as_bytes());
        let tools = responses[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for forbidden in &[
            "fire",
            "ccplan_fire",
            "mcp",
            "ccplan_mcp",
            "completions",
            "ccplan_completions",
        ] {
            assert!(
                !names.contains(forbidden),
                "tool catalog must not expose {forbidden}"
            );
        }
    }

    #[test]
    fn unknown_tool_returns_error() {
        let (_temp, context) = test_context();
        let mut input = notif("notifications/initialized");
        input.push_str(&req(
            1,
            "tools/call",
            json!({"name": "ccplan_fire", "arguments": {}}),
        ));
        let responses = run_serve(&context, input.as_bytes());
        assert_eq!(responses[0]["result"]["isError"], true);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.contains("unknown tool"),
            "expected 'unknown tool' error, got: {text}"
        );
    }

    #[test]
    fn store_locked_error_has_store_locked_code() {
        let e = Error::Store(StoreError::Locked);
        assert_eq!(super::error_code(&e), "store_locked");
    }

    #[test]
    fn store_locked_error_hint_suggests_retry() {
        let e = Error::Store(StoreError::Locked);
        let hint = super::error_hint(&e);
        assert!(
            hint.contains("retry"),
            "hint should mention retry, got: {hint}"
        );
    }

    #[test]
    fn run_authorization_warning_empty_runs_returns_none() {
        let automation = AutomationConfig::default();
        assert!(super::run_authorization_warning(&automation, &[]).is_none());
    }

    #[test]
    fn run_authorization_warning_automation_disabled_returns_warning() {
        let automation = AutomationConfig::default();
        let run = vec!["notify-send".to_owned(), "hello".to_owned()];
        let result = super::run_authorization_warning(&automation, &[&run]);
        assert!(result.is_some(), "should warn when automation is disabled");
        let msg = result.unwrap();
        assert!(msg.contains("automation is disabled"), "got: {msg}");
        assert!(msg.contains("notification will still fire"), "got: {msg}");
    }

    #[test]
    fn run_authorization_warning_unlisted_executable_returns_warning() {
        let automation = AutomationConfig {
            enabled: true,
            allowed_executables: vec![PathBuf::from("/usr/bin/allowed")],
            ..Default::default()
        };
        let run = vec!["/usr/bin/unlisted".to_owned(), "arg".to_owned()];
        let result = super::run_authorization_warning(&automation, &[&run]);
        assert!(result.is_some(), "should warn for unlisted executable");
        let msg = result.unwrap();
        assert!(msg.contains("/usr/bin/unlisted"), "got: {msg}");
        assert!(msg.contains("not in the allowlist"), "got: {msg}");
    }

    #[test]
    fn run_authorization_warning_allowlisted_executable_returns_none() {
        let automation = AutomationConfig {
            enabled: true,
            allowed_executables: vec![PathBuf::from("/usr/bin/allowed")],
            ..Default::default()
        };
        let run = vec!["/usr/bin/allowed".to_owned()];
        assert!(
            super::run_authorization_warning(&automation, &[&run]).is_none(),
            "no warning when executable is allowlisted"
        );
    }

    #[test]
    fn plan_day_with_run_block_and_automation_disabled_emits_warning() {
        let (_temp, ctx) = test_context();
        let mut input = notif("notifications/initialized");
        input.push_str(&req(
            1,
            "tools/call",
            json!({
                "name": "ccplan_plan_day",
                "arguments": {
                    "timezone": "Asia/Kolkata",
                    "blocks": [{
                        "title": "Scripted Block",
                        "start": "10:00",
                        "duration": "1h",
                        "run": ["/usr/bin/notify-send", "hello"]
                    }]
                }
            }),
        ));
        let responses = run_serve(&ctx, input.as_bytes());
        let tool_text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            tool_text.contains("WARNING"),
            "should warn about disabled automation; got: {tool_text}"
        );
        assert!(
            tool_text.contains("automation is disabled"),
            "got: {tool_text}"
        );
    }

    #[test]
    fn add_block_with_run_and_automation_disabled_emits_warning() {
        let (_temp, ctx) = test_context();
        let plan_input = req(
            1,
            "tools/call",
            json!({
                "name": "ccplan_plan_day",
                "arguments": {
                    "timezone": "Asia/Kolkata",
                    "blocks": [{"title": "Seed", "start": "10:00", "duration": "1h"}]
                }
            }),
        );
        let add_input = req(
            2,
            "tools/call",
            json!({
                "name": "ccplan_add_block",
                "arguments": {
                    "title": "Shell Block",
                    "start": "11:00",
                    "duration": "30m",
                    "run": ["/usr/local/bin/sync.sh"]
                }
            }),
        );
        let mut input = notif("notifications/initialized");
        input.push_str(&plan_input);
        input.push_str(&add_input);
        let responses = run_serve(&ctx, input.as_bytes());
        let tool_text = responses[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            tool_text.contains("WARNING"),
            "should warn about disabled automation; got: {tool_text}"
        );
    }
}
