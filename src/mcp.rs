//! Hand-rolled synchronous JSON-RPC 2.0 MCP server over stdio.
//!
//! Protocol: newline-delimited JSON-RPC 2.0 (one UTF-8 JSON object per line).
//! Transport: stdio. The real stdin/stdout handles are injected by the single
//! coverage-off wrapper [`run_mcp_server`]; the core [`serve`] uses dynamic
//! dispatch so there is exactly one instantiation and tests drive it with
//! in-memory buffers.

use std::io::{BufRead, Write};

use jiff::SignedDuration;
use serde_json::{Value, json};

use crate::{
    cli::{ApplyArgs, Commands, ReadArgs},
    commands::{self, set_from_str, slug_block_id},
    config::Config,
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

    let plan_date = date.unwrap_or_else(|| PlanDate::from_jiff_date(context.clock.now().date()));
    let plan = Plan {
        date: plan_date,
        timezone,
        blocks,
    };
    let toml = plan.to_toml().map_err(Error::from)?;

    let mut out = Vec::new();
    set_from_str(&toml, None, override_history, &mut out, context)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
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
        _ => "check the error message for details",
    }
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
    vec![plan_day_schema(), apply_schema(), show_plan_schema()]
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use assert_fs::TempDir;
    use jiff::Zoned;
    use serde_json::{Value, json};

    use crate::{
        config::Config,
        context::{Context, RecordingNotifier, RecordingScheduler},
        model::{
            Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Span, Status,
            TimeZoneName,
        },
        store::{HistoryPolicy, Store},
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
}
