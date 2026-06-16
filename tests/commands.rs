use assert_fs::TempDir;
use ccplan::{
    cli::Cli,
    config::Config,
    context::{
        Context, Notification, Notifier, NotifyError, RecordingNotifier, RecordingScheduler,
        Scheduler, SchedulerCall, SchedulerError,
    },
    error::Error,
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName,
    },
    run_with_context,
    store::{HistoryPolicy, Store, StoreError, TriggerRecord},
    time::FixedClock,
};
use clap::Parser;
use jiff::{SignedDuration, Timestamp, Zoned};
use serde_json::Value;

#[test]
fn no_command_is_a_successful_noop() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    let output = run_ok(&context, ["ccplan"]);

    assert!(output.is_empty());
}

#[test]
fn mcp_subcommand_exits_cleanly_on_eof() {
    // In test builds run_mcp_server uses an empty Cursor instead of real stdin,
    // so this exits immediately and produces no output.
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let output = run_ok(&context, ["ccplan", "mcp"]);
    assert!(output.is_empty());
}

#[test]
fn watch_renders_a_frame_then_quits_on_eof() {
    // Under the test harness stdin is closed, so watch's input-reader thread signals quit right
    // after the first frame — this drives the real timer/input loop end-to-end without hanging,
    // the same way `mcp` exits on EOF.
    let (_temp, context) = test_context_at("2026-06-08T10:50:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    let output = String::from_utf8(run_ok(&context, ["ccplan", "watch", "--every", "1s"])).unwrap();

    assert!(output.contains("ccplan watch ·"), "watch frame: {output}");
    assert!(output.contains("Focus time"), "watch frame: {output}");
}

#[test]
fn set_and_show_json_round_trip_with_fake_context() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let input = context
        .store
        .plan_path(&date())
        .with_file_name("incoming.toml");
    std::fs::create_dir_all(input.parent().unwrap()).unwrap();
    std::fs::write(&input, plan_toml()).unwrap();

    run_ok(
        &context,
        ["ccplan", "set", "--from", input.to_str().unwrap()],
    );

    let output = run_ok(&context, ["ccplan", "show", "--json"]);
    let json: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "block": [
                {
                    "end": "11:30",
                    "id": "focus",
                    "notify": "5m",
                    "start": "11:00",
                    "status": "pending",
                    "tags": ["deep-work"],
                    "title": "Focus time",
                },
                {
                    "duration": "30m",
                    "id": "lunch",
                    "notify": "5m",
                    "start": "14:00",
                    "status": "pending",
                    "tags": [],
                    "title": "Lunch",
                },
            ],
            "date": "2026-06-08",
            "timezone": "Asia/Kolkata",
        })
    );
}

#[test]
fn set_date_override_and_override_history_replace_terminal_history() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let input = context
        .store
        .plan_path(&date())
        .with_file_name("incoming-override.toml");
    std::fs::create_dir_all(input.parent().unwrap()).unwrap();
    std::fs::write(&input, plan_toml()).unwrap();

    run_ok(
        &context,
        [
            "ccplan",
            "set",
            "--from",
            input.to_str().unwrap(),
            "--date",
            "2026-06-09",
        ],
    );

    assert!(context.store.load_plan(&date()).unwrap().is_none());
    assert!(
        context
            .store
            .load_plan(&"2026-06-09".parse().unwrap())
            .unwrap()
            .is_some()
    );

    let terminal = Plan {
        date: date(),
        timezone: "Asia/Kolkata".parse().unwrap(),
        blocks: vec![{
            let mut block = block_with(
                "focus",
                "Finished",
                "10:00",
                Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
            );
            block.status = Status::Done;
            block
        }],
    };
    context
        .store
        .set_plan(&terminal, HistoryPolicy::Override)
        .unwrap();
    run_ok(
        &context,
        [
            "ccplan",
            "set",
            "--from",
            input.to_str().unwrap(),
            "--override-history",
        ],
    );

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].status, Status::Pending);
}

#[test]
fn mutation_commands_update_only_non_terminal_blocks() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--id",
            "focus",
            "--title",
            "Focus",
            "--start",
            "11:00",
            "--duration",
            "30m",
        ],
    );
    run_ok(
        &context,
        [
            "ccplan",
            "edit",
            "focus",
            "--title",
            "Deep Focus",
            "--start",
            "11:15",
        ],
    );
    run_ok(&context, ["ccplan", "done", "focus"]);

    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--id",
            "break",
            "--title",
            "Break",
            "--start",
            "12:00",
            "--duration",
            "15m",
        ],
    );
    run_ok(&context, ["ccplan", "skip", "break"]);

    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--id",
            "tmp",
            "--title",
            "Temporary",
            "--start",
            "13:00",
            "--duration",
            "15m",
        ],
    );
    run_ok(&context, ["ccplan", "rm", "tmp"]);

    let plan = context.store.load_plan(&date()).unwrap().unwrap();

    assert_eq!(plan.blocks.len(), 2);
    assert_eq!(plan.blocks[0].id.as_str(), "focus");
    assert_eq!(plan.blocks[0].title, "Deep Focus");
    assert_eq!(plan.blocks[0].start.to_string(), "11:15");
    assert_eq!(plan.blocks[0].status, Status::Done);
    assert_eq!(plan.blocks[1].status, Status::Skipped);
}

#[test]
fn add_edit_and_status_errors_cover_conflict_paths() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--title",
            "Deep Focus!",
            "--start",
            "11:00",
            "--duration",
            "30m",
            "--tags",
            "deep,agent",
        ],
    );
    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--id",
            "deep-focus",
            "--title",
            "Renamed Focus",
            "--start",
            "11:10",
            "--duration",
            "20m",
        ],
    );
    run_ok(
        &context,
        [
            "ccplan",
            "edit",
            "deep-focus",
            "--end",
            "11:40",
            "--notify",
            "10m",
            "--run",
            "/bin/echo",
            "hello",
        ],
    );
    run_ok(
        &context,
        ["ccplan", "edit", "deep-focus", "--duration", "45m"],
    );

    let err = run_err(
        &context,
        [
            "ccplan",
            "edit",
            "deep-focus",
            "--end",
            "12:00",
            "--duration",
            "30m",
        ],
    );
    assert_eq!(err.exit_code(), 2);

    run_ok(&context, ["ccplan", "done", "deep-focus"]);
    let err = run_err(
        &context,
        [
            "ccplan",
            "add",
            "--id",
            "deep-focus",
            "--title",
            "Reuse",
            "--start",
            "13:00",
            "--duration",
            "30m",
        ],
    );
    assert_eq!(err.exit_code(), 6);
    let err = run_err(&context, ["ccplan", "skip", "deep-focus"]);
    assert_eq!(err.exit_code(), 6);
    let err = run_err(
        &context,
        ["ccplan", "edit", "missing", "--title", "Missing"],
    );
    assert_eq!(err.exit_code(), 3);
    let err = run_err(&context, ["ccplan", "rm", "missing"]);
    assert_eq!(err.exit_code(), 3);

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].id.as_str(), "deep-focus");
    assert_eq!(stored.blocks[0].title, "Renamed Focus");
    assert!(stored.blocks[0].tags.is_empty());
    assert_eq!(
        stored.blocks[0].run.as_ref().map(Run::as_slice),
        Some(["/bin/echo".to_owned(), "hello".to_owned()].as_slice())
    );

    let (_temp, empty_context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let err = run_err(&empty_context, ["ccplan", "show", "--date", "2026-06-08"]);
    assert_eq!(err.exit_code(), 3);

    // Mutations against a date with no plan at all must report NotFound (exit 3) — this exercises
    // the transactional update's "no existing plan" branch (required_plan's None case).
    for command in [
        vec!["ccplan", "done", "ghost"],
        vec!["ccplan", "edit", "ghost", "--title", "Ghost"],
        vec!["ccplan", "rm", "ghost"],
    ] {
        let err = run_err(&empty_context, command);
        assert_eq!(err.exit_code(), 3);
    }
}

#[test]
fn remind_schedules_zero_lead_block_and_auto_applies() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    let output = String::from_utf8(run_ok(
        &context,
        ["ccplan", "remind", "Stretch break", "--in", "1h30m"],
    ))
    .unwrap();

    // Human confirmation reports the resolved target (10:00 + 1h30m = 11:30, today).
    assert!(output.contains("reminder \"Stretch break\" set for 11:30 on 2026-06-08"));

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks.len(), 1);
    let block = &stored.blocks[0];
    assert_eq!(block.id.as_str(), "stretch-break"); // auto-slugged from the text
    assert_eq!(block.title, "Stretch break");
    assert_eq!(block.start.to_string(), "11:30");
    assert_eq!(block.span, Span::Duration("1m".parse().unwrap()));
    assert_eq!(block.notify.as_seconds(), 0); // zero lead

    // Auto-applied: the OS triggers are live without a second `apply`. A zero-lead block fires only
    // its `start` (which itself notifies) and `end` — the heads-up `notify` trigger is omitted.
    let triggers = context.store.list_triggers().unwrap();
    assert_eq!(triggers.len(), 2);
    assert!(triggers.iter().any(|t| t.backend_id.ends_with("-start")));
    assert!(triggers.iter().any(|t| t.backend_id.ends_with("-end")));
    assert!(!triggers.iter().any(|t| t.backend_id.ends_with("-notify")));
    assert_eq!(context.scheduler.triggers().len(), 2);

    // Re-using the slugged id while the block is still pending replaces it (no error).
    run_ok(
        &context,
        ["ccplan", "remind", "Stretch break", "--in", "2h"],
    );
    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks.len(), 1);
    assert_eq!(stored.blocks[0].start.to_string(), "12:00");

    // Once the block is terminal, re-using its id is a history conflict (exit 6).
    run_ok(&context, ["ccplan", "done", "stretch-break"]);
    let err = run_err(
        &context,
        ["ccplan", "remind", "Stretch break", "--in", "3h"],
    );
    assert_eq!(err.exit_code(), 6);
}

#[test]
fn remind_rolls_over_to_next_day_and_honors_explicit_id() {
    // 23:00 + 2h crosses midnight: the reminder must land in tomorrow's plan, not today's.
    let (_temp, context) = test_context_at("2026-06-08T23:00:00+05:30[Asia/Kolkata]");

    run_ok(
        &context,
        ["ccplan", "remind", "Call mom", "--in", "2h", "--id", "ring"],
    );

    let tomorrow: PlanDate = "2026-06-09".parse().unwrap();
    assert!(context.store.load_plan(&date()).unwrap().is_none());
    let stored = context.store.load_plan(&tomorrow).unwrap().unwrap();
    assert_eq!(stored.blocks.len(), 1);
    assert_eq!(stored.blocks[0].id.as_str(), "ring"); // explicit --id wins over the slug
    assert_eq!(stored.blocks[0].start.to_string(), "01:00");
}

#[test]
fn read_queries_reconcile_in_memory_without_persisting_and_apply_persists() {
    let (_temp, context) = test_context_at("2026-06-08T11:10:00+05:30[Asia/Kolkata]");
    let mut plan = plan();
    plan.blocks[0].status = Status::Active;
    plan.blocks.push(block_with(
        "stale",
        "Stale pending",
        "09:00",
        Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
    ));
    context
        .store
        .set_plan(&plan, HistoryPolicy::Preserve)
        .unwrap();

    let now: Value =
        serde_json::from_slice(&run_ok(&context, ["ccplan", "now", "--json"])).unwrap();
    let next: Value =
        serde_json::from_slice(&run_ok(&context, ["ccplan", "next", "--json"])).unwrap();
    let agenda: Value =
        serde_json::from_slice(&run_ok(&context, ["ccplan", "agenda", "--json"])).unwrap();

    assert_eq!(
        now,
        serde_json::json!([
            {
                "end": "11:30",
                "id": "focus",
                "start": "11:00",
                "status": "active",
                "title": "Focus time",
            }
        ])
    );
    assert_eq!(next.as_array().unwrap()[0]["id"], "lunch");
    assert!(
        agenda
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["id"] == "lunch")
    );

    // Inv-18: the three reads above reconciled the overdue `stale` block in memory only — the
    // stored plan is byte-for-byte unchanged, so `stale` keeps its on-disk Pending status.
    let stale_status = |store: &Store| {
        store
            .load_plan(&date())
            .unwrap()
            .unwrap()
            .blocks
            .into_iter()
            .find(|block| block.id.as_str() == "stale")
            .unwrap()
            .status
    };
    assert_eq!(stale_status(&context.store), Status::Pending);

    // A dry-run apply is also a preview and must not persist reconciliation.
    run_ok(&context, ["ccplan", "apply", "--dry-run"]);
    assert_eq!(stale_status(&context.store), Status::Pending);

    // A real `apply` is a mutation point and DOES persist the overdue reconciliation.
    run_ok(&context, ["ccplan", "apply"]);
    assert_eq!(stale_status(&context.store), Status::Missed);
}

#[test]
fn human_read_outputs_cover_empty_and_non_empty_arrays() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    let show = String::from_utf8(run_ok(&context, ["ccplan", "show"])).unwrap();
    let now = String::from_utf8(run_ok(&context, ["ccplan", "now"])).unwrap();
    let agenda = String::from_utf8(run_ok(&context, ["ccplan", "agenda"])).unwrap();

    assert!(show.contains("[[block]]"));
    // At 10:00 nothing is active: empty reads print plain language, not "[]".
    assert_eq!(now.trim(), "no active blocks right now");
    // The agenda renders a scannable, headed human table (not "N item(s)"), with countdowns.
    assert!(agenda.contains("TIME"));
    assert!(agenda.contains("IN"));
    assert!(agenda.contains("STATUS"));
    assert!(agenda.contains("11:00-11:30"));
    assert!(agenda.contains("in 1h00m"));
    assert!(agenda.contains("focus"));
    assert!(agenda.contains("Focus time"));
    assert!(agenda.contains("in 4h00m"));
    assert!(agenda.contains("Lunch"));
    assert!(agenda.contains("pending"));

    // A read must not have persisted anything: reconciliation here is in-memory only (Inv-18).
    let after_reads = std::fs::read(context.store.plan_path(&date())).unwrap();
    let _ = String::from_utf8(run_ok(&context, ["ccplan", "agenda"])).unwrap();
    assert_eq!(
        std::fs::read(context.store.plan_path(&date())).unwrap(),
        after_reads,
        "read commands must leave the plan file byte-identical"
    );

    // Active-block human output (a separate context where `focus` is in progress).
    let (_temp_active, active) = test_context_at("2026-06-08T11:15:00+05:30[Asia/Kolkata]");
    let mut active_plan = plan();
    active_plan.blocks[0].status = Status::Active;
    active
        .store
        .set_plan(&active_plan, HistoryPolicy::Preserve)
        .unwrap();
    let now_active = String::from_utf8(run_ok(&active, ["ccplan", "now"])).unwrap();
    assert!(now_active.contains("focus"));
    assert!(now_active.contains("Focus time"));
    assert!(now_active.contains("active"));
    assert!(now_active.contains("11:00-11:30"));

    let finished = Plan {
        date: date(),
        timezone: "Asia/Kolkata".parse().unwrap(),
        blocks: vec![{
            let mut block = block_with(
                "done",
                "Done",
                "09:00",
                Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
            );
            block.status = Status::Done;
            block
        }],
    };
    context
        .store
        .set_plan(&finished, HistoryPolicy::Override)
        .unwrap();
    let next: serde_json::Value =
        serde_json::from_slice(&run_ok(&context, ["ccplan", "next", "--json"])).unwrap();
    assert_eq!(next.as_array().unwrap().len(), 0);
}

#[test]
fn apply_dry_run_reports_diff_and_real_apply_is_idempotent() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    let dry_run = String::from_utf8(run_ok(&context, ["ccplan", "apply", "--dry-run"])).unwrap();

    assert!(dry_run.contains("add "));
    assert!(context.scheduler.calls().is_empty());
    assert!(context.store.list_triggers().unwrap().is_empty());

    run_ok(&context, ["ccplan", "apply"]);

    assert_eq!(context.scheduler.calls().len(), 6);
    assert_eq!(context.scheduler.triggers().len(), 6);
    assert_eq!(context.store.list_triggers().unwrap().len(), 6);
    context.scheduler.clear_calls();

    run_ok(&context, ["ccplan", "apply"]);

    assert!(context.scheduler.calls().is_empty());
}

#[test]
fn clear_requires_yes_and_purge_removes_without_archive() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    let err = run_err(&context, ["ccplan", "clear"]);
    assert_eq!(err.exit_code(), 2);

    run_ok(&context, ["ccplan", "clear", "--yes", "--purge"]);

    assert!(context.store.load_plan(&date()).unwrap().is_none());
    assert!(!context.store.archive_path(&date()).exists());
}

#[test]
fn remaining_command_branches_are_covered() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    run_ok(
        &context,
        [
            "ccplan",
            "add",
            "--title",
            "!!!",
            "--start",
            "11:00",
            "--end",
            "11:30",
            "--run",
            "/bin/echo",
        ],
    );
    assert_eq!(
        context.store.load_plan(&date()).unwrap().unwrap().blocks[0]
            .id
            .as_str(),
        "block"
    );
    assert_eq!(
        run_err(
            &context,
            ["ccplan", "add", "--title", "Bad", "--start", "12:00"]
        )
        .exit_code(),
        2
    );
    assert_eq!(
        run_err(
            &context,
            [
                "ccplan",
                "add",
                "--title",
                "Bad",
                "--start",
                "12:00",
                "--end",
                "12:30",
                "--duration",
                "30m",
            ],
        )
        .exit_code(),
        2
    );
    run_ok(&context, ["ccplan", "done", "block"]);
    assert_eq!(run_err(&context, ["ccplan", "rm", "block"]).exit_code(), 6);

    let (_temp, dry_clear) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    dry_clear
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    run_ok(&dry_clear, ["ccplan", "apply"]);
    run_ok(&dry_clear, ["ccplan", "clear", "--yes", "--dry-run"]);
    assert!(dry_clear.store.load_plan(&date()).unwrap().is_some());

    let (_temp, terminal_apply) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let mut terminal_plan = plan();
    for block in &mut terminal_plan.blocks {
        block.status = Status::Done;
    }
    terminal_apply
        .store
        .set_plan(&terminal_plan, HistoryPolicy::Preserve)
        .unwrap();
    assert_eq!(
        String::from_utf8(run_ok(&terminal_apply, ["ccplan", "apply"]))
            .unwrap()
            .trim(),
        "no changes"
    );

    let (_temp, no_op_fire) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    no_op_fire
        .store
        .set_plan(&terminal_plan, HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&no_op_fire);
    run_ok(
        &no_op_fire,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        std::fs::read_to_string(no_op_fire.store.fire_log_path())
            .unwrap()
            .contains("no-op")
    );

    let (_temp, seconds_context) = test_context_at("2026-06-08T11:00:30+05:30[Asia/Kolkata]");
    let mut seconds_plan = Plan {
        date: date(),
        timezone: "Asia/Kolkata".parse().unwrap(),
        blocks: vec![block_with(
            "seconds",
            "Seconds",
            "11:00",
            Span::Duration(DurationSpec::from_seconds(90).unwrap()),
        )],
    };
    seconds_plan.blocks[0].status = Status::Active;
    seconds_context
        .store
        .set_plan(&seconds_plan, HistoryPolicy::Preserve)
        .unwrap();
    let now: serde_json::Value =
        serde_json::from_slice(&run_ok(&seconds_context, ["ccplan", "now", "--json"])).unwrap();
    assert_eq!(now.as_array().unwrap()[0]["end"], "11:01:30");

    let (_temp, agenda_edge) = test_context_at("2026-06-08T11:30:00+05:30[Asia/Kolkata]");
    let mut agenda_plan = plan();
    agenda_plan.blocks[0].status = Status::Active;
    agenda_edge
        .store
        .set_plan(&agenda_plan, HistoryPolicy::Preserve)
        .unwrap();
    let agenda: serde_json::Value =
        serde_json::from_slice(&run_ok(&agenda_edge, ["ccplan", "agenda", "--json"])).unwrap();
    assert!(
        agenda
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["id"] != "focus")
    );
}

#[test]
fn clear_uses_reconciler_to_remove_triggers_then_archives_plan() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    run_ok(&context, ["ccplan", "apply"]);
    context.scheduler.clear_calls();

    run_ok(&context, ["ccplan", "clear", "--yes"]);

    assert_eq!(context.store.load_plan(&date()).unwrap(), None);
    assert!(context.store.archive_path(&date()).exists());
    assert_eq!(context.store.list_triggers().unwrap(), Vec::new());
    assert!(
        context
            .scheduler
            .calls()
            .iter()
            .all(|call| matches!(call, SchedulerCall::Remove(_)))
    );
}

#[test]
fn fire_covers_missing_notify_missed_close_and_run_deferred_paths() {
    let (_temp, missing) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    run_ok(
        &missing,
        fire_args("focus", "start", "0000000000000000", "2026-06-08T05:30:00Z"),
    );

    let (_temp, no_block) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    no_block
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    run_ok(
        &no_block,
        fire_args(
            "missing",
            "start",
            "0000000000000000",
            "2026-06-08T05:30:00Z",
        ),
    );

    let (_temp, notify_context) = test_context_at("2026-06-08T10:55:00+05:30[Asia/Kolkata]");
    notify_context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&notify_context);
    run_ok(
        &notify_context,
        fire_args("focus", "notify", rev.as_str(), "2026-06-08T05:25:00Z"),
    );
    assert_eq!(notify_context.notifier.notifications().len(), 1);

    let (_temp, mut run_context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    let exe = if cfg!(windows) {
        "C:\\Windows\\System32\\cmd.exe"
    } else {
        "/bin/echo"
    };
    run_context.config.automation.enabled = true;
    run_context.config.automation.allowed_executables = vec![std::path::PathBuf::from(exe)];
    let mut run_plan = plan();
    let run_argv = if cfg!(windows) {
        vec![exe.to_owned(), "/c".to_owned(), "echo".to_owned()]
    } else {
        vec![exe.to_owned()]
    };
    run_plan.blocks[0].run = Some(Run::new(run_argv).unwrap());
    run_context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = run_context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }
    let rev = focus_rev(&run_context);
    run_ok(
        &run_context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        std::fs::read_to_string(run_context.store.fire_log_path())
            .unwrap()
            .contains("activated run")
    );

    let (_temp, missed_context) = test_context_at("2026-06-08T11:02:00+05:30[Asia/Kolkata]");
    missed_context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&missed_context);
    run_ok(
        &missed_context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert_eq!(
        missed_context
            .store
            .load_plan(&date())
            .unwrap()
            .unwrap()
            .blocks[0]
            .status,
        Status::Missed
    );

    let (_temp, close_context) = test_context_at("2026-06-08T11:30:00+05:30[Asia/Kolkata]");
    let mut active = plan();
    active.blocks[0].status = Status::Active;
    close_context
        .store
        .set_plan(&active, HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&close_context);
    run_ok(
        &close_context,
        fire_args("focus", "end", rev.as_str(), "2026-06-08T06:00:00Z"),
    );
    assert_eq!(
        close_context
            .store
            .load_plan(&date())
            .unwrap()
            .unwrap()
            .blocks[0]
            .status,
        Status::Expired
    );
}

#[test]
fn fire_start_records_ledger_notifies_updates_status_and_deduplicates() {
    let (_temp, context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = context.store.load_plan(&date()).unwrap().unwrap().blocks[0].schedule_rev();

    let args = [
        "ccplan",
        "fire",
        "--date",
        "2026-06-08",
        "--id",
        "focus",
        "--event",
        "start",
        "--rev",
        rev.as_str(),
        "--at",
        "2026-06-08T05:30:00Z",
    ];
    run_ok(&context, args);
    run_ok(&context, args);

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].status, Status::Active);
    assert_eq!(context.notifier.notifications().len(), 1);
    let logged = std::fs::read_to_string(context.store.fire_log_path()).unwrap();
    assert!(logged.contains("\"id\":\"focus\""));
    assert!(logged.contains("\"event\":\"start\""));
}

#[test]
fn notification_failures_warn_on_apply_and_are_logged_on_fire() {
    let (_temp, context) = failing_notifier_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    let apply = String::from_utf8(run_ok(&context, ["ccplan", "apply"])).unwrap();
    assert!(apply.contains("warning: notifier: notification failed: no desktop bus"));

    let rev = focus_rev(&context);
    run_ok(
        &context,
        fire_args("focus", "notify", rev.as_str(), "2026-06-08T05:25:00Z"),
    );

    assert!(
        std::fs::read_to_string(context.store.fire_log_path())
            .unwrap()
            .contains("notify-failed=notification_failed:_send_failed")
    );
}

#[test]
fn stale_fire_noops_without_recording_a_notification() {
    let (_temp, context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    run_ok(
        &context,
        [
            "ccplan",
            "fire",
            "--date",
            "2026-06-08",
            "--id",
            "focus",
            "--event",
            "start",
            "--rev",
            "0000000000000000",
            "--at",
            "2026-06-08T05:30:00Z",
        ],
    );

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].status, Status::Pending);
    assert!(context.notifier.notifications().is_empty());
}

#[test]
fn status_doctor_and_completions_are_non_interactive() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    assert!(
        String::from_utf8(run_ok(&context, ["ccplan", "status"]))
            .unwrap()
            .contains("triggers")
    );
    assert!(
        String::from_utf8(run_ok(&context, ["ccplan", "doctor"]))
            .unwrap()
            .contains("scheduler")
    );
    assert!(
        String::from_utf8(run_ok(&context, ["ccplan", "completions", "bash"]))
            .unwrap()
            .contains("complete -F")
    );
}

#[test]
fn status_reports_scheduler_list_failures_without_failing() {
    let (_temp, context) =
        list_failing_scheduler_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");

    let output = String::from_utf8(run_ok(&context, ["ccplan", "status"])).unwrap();

    assert!(output.contains("live triggers: unavailable"));
}

#[test]
fn apply_omits_notify_trigger_at_zero_lead_but_keeps_it_for_positive_lead() {
    // Inv-16 (no double-notify): a block whose notify lead is 0 has its notify instant coincide
    // with `start`, so apply schedules only start+end — the start event carries the single
    // notification. A block with a positive lead also gets a distinct, earlier notify trigger.
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    let plan = Plan {
        date: date(),
        timezone: "Asia/Kolkata".parse().unwrap(),
        blocks: vec![
            {
                let mut block = block_with(
                    "zero",
                    "Zero lead",
                    "11:00",
                    Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
                );
                block.notify = Lead::from_seconds(0).unwrap();
                block
            },
            {
                let mut block = block_with(
                    "lead",
                    "With lead",
                    "12:00",
                    Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
                );
                block.notify = Lead::from_seconds(10 * 60).unwrap();
                block
            },
        ],
    };
    context
        .store
        .set_plan(&plan, HistoryPolicy::Preserve)
        .unwrap();

    run_ok(&context, ["ccplan", "apply"]);

    let added = context
        .scheduler
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            SchedulerCall::Add(id) => Some(id),
            SchedulerCall::Remove(_) => None,
        })
        .collect::<Vec<_>>();
    let notify_triggers = added.iter().filter(|id| id.ends_with("-notify")).count();
    // zero-lead: start+end (2); positive-lead: start+end+notify (3).
    assert_eq!(added.len(), 5, "{added:?}");
    assert_eq!(
        notify_triggers, 1,
        "only the positive-lead block gets a separate notify trigger: {added:?}"
    );
}

#[test]
fn store_update_serializes_concurrent_additive_writes_without_loss() {
    use std::sync::Arc;

    // Inv-17: Store::update holds the lock across load->mutate->write, so two writers each
    // appending a different block to the same day cannot lose each other's work. Eight threads
    // race to add distinct, non-overlapping blocks; on lock contention a writer simply retries.
    let temp = TempDir::new().unwrap();
    let store = Arc::new(Store::new(temp.path()));
    let day = date();
    store
        .set_plan(
            &Plan {
                date: day.clone(),
                timezone: "Asia/Kolkata".parse().unwrap(),
                blocks: Vec::new(),
            },
            HistoryPolicy::Preserve,
        )
        .unwrap();

    let handles = (0..8u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let day = day.clone();
            std::thread::spawn(move || {
                let block = block_with(
                    &format!("b{i}"),
                    "Concurrent",
                    &format!("{:02}:00", 8 + i),
                    Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
                );
                loop {
                    let block = block.clone();
                    let outcome =
                        store.update(&day, Lead::from_seconds(0).unwrap(), move |existing| {
                            let mut plan = existing.expect("seed plan is present");
                            plan.blocks.push(block);
                            Ok::<_, StoreError>(plan)
                        });
                    match outcome {
                        Ok(_) => break,
                        Err(StoreError::Locked) => std::thread::yield_now(),
                        Err(error) => panic!("unexpected store error: {error}"),
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let stored = store.load_plan(&day).unwrap().unwrap();
    assert_eq!(
        stored.blocks.len(),
        8,
        "no concurrent additive write was lost"
    );
}

fn run_ok<C, S, N, I, T>(context: &Context<C, S, N>, args: I) -> Vec<u8>
where
    C: ccplan::time::Clock,
    S: ccplan::context::Scheduler,
    N: ccplan::context::Notifier,
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let mut output = Vec::new();
    run_with_context(cli, &mut output, context).expect("command should succeed");
    output
}

fn run_err<C, S, N, I, T>(context: &Context<C, S, N>, args: I) -> Error
where
    C: ccplan::time::Clock,
    S: ccplan::context::Scheduler,
    N: ccplan::context::Notifier,
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let mut output = Vec::new();
    run_with_context(cli, &mut output, context).expect_err("command should fail")
}

fn focus_rev<C, S, N>(context: &Context<C, S, N>) -> ccplan::model::ScheduleRev {
    context.store.load_plan(&date()).unwrap().unwrap().blocks[0].schedule_rev()
}

#[test]
fn fire_log_reads_the_ledger_with_filters() {
    // Empty ledger: nothing has fired yet.
    let (_empty_temp, empty) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    assert_eq!(
        String::from_utf8(run_ok(&empty, ["ccplan", "log"]))
            .unwrap()
            .trim(),
        "no fires recorded"
    );

    // Fire a real start event so the ledger holds one activate record.
    let (_temp, context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&context);
    run_ok(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );

    // Human table carries the block id and outcome.
    let human = String::from_utf8(run_ok(&context, ["ccplan", "log"])).unwrap();
    assert!(human.contains("focus"), "human log: {human}");
    assert!(human.contains("activate"), "human log: {human}");

    // JSON output is structured.
    let json = String::from_utf8(run_ok(&context, ["ccplan", "log", "--json"])).unwrap();
    assert!(
        json.contains("\"outcome\": \"activate\""),
        "json log: {json}"
    );

    // --date keeps a matching date, drops a non-matching one.
    let matched = String::from_utf8(run_ok(
        &context,
        ["ccplan", "log", "--date", "2026-06-08", "--json"],
    ))
    .unwrap();
    assert!(matched.contains("\"id\": \"focus\""), "matched: {matched}");
    assert_eq!(
        String::from_utf8(run_ok(&context, ["ccplan", "log", "--date", "2026-06-09"]))
            .unwrap()
            .trim(),
        "no fires recorded"
    );

    // --since keeps fires at/after the instant, drops earlier ones.
    let since_before = String::from_utf8(run_ok(
        &context,
        ["ccplan", "log", "--since", "2026-06-08T05:00:00Z", "--json"],
    ))
    .unwrap();
    assert!(
        since_before.contains("\"outcome\": \"activate\""),
        "since_before: {since_before}"
    );
    assert_eq!(
        String::from_utf8(run_ok(
            &context,
            ["ccplan", "log", "--since", "2026-06-08T06:00:00Z"]
        ))
        .unwrap()
        .trim(),
        "no fires recorded"
    );
}

#[test]
fn snooze_slides_blocks_later_reapplies_and_refuses_rollover() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();

    // End-span block: both start and end slide, preserving the 30-minute length.
    let message = String::from_utf8(run_ok(
        &context,
        ["ccplan", "snooze", "focus", "--by", "1h"],
    ))
    .unwrap();
    assert!(message.contains("snoozed focus by"), "{message}");
    assert!(message.contains("2026-06-08"), "{message}");
    // The re-apply armed native triggers for the moved block.
    assert!(!context.scheduler.calls().is_empty());

    // Duration-span block: only start slides; the duration is untouched.
    run_ok(&context, ["ccplan", "snooze", "lunch", "--by", "2h"]);

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    let focus = &stored.blocks[0];
    assert_eq!(focus.start.to_string(), "12:00");
    assert_eq!(focus.span, Span::End("12:30".parse::<ClockTime>().unwrap()));
    let lunch = &stored.blocks[1];
    assert_eq!(lunch.start.to_string(), "16:00");
    assert_eq!(
        lunch.span,
        Span::Duration(DurationSpec::from_seconds(1800).unwrap())
    );

    // A slide that would cross midnight is refused (NG8: no day rollover).
    let rollover = run_err(&context, ["ccplan", "snooze", "lunch", "--by", "24h"]);
    assert!(matches!(rollover, Error::Usage(message) if message.contains("past midnight")));

    // Terminal blocks and unknown ids are refused like the other mutations.
    run_ok(&context, ["ccplan", "done", "focus"]);
    assert!(matches!(
        run_err(&context, ["ccplan", "snooze", "focus", "--by", "5m"]),
        Error::HistoryConflict { .. }
    ));
    assert!(matches!(
        run_err(&context, ["ccplan", "snooze", "ghost", "--by", "5m"]),
        Error::NotFound(_)
    ));
}

#[test]
fn template_save_list_apply_round_trip_and_validation() {
    let (_temp, context) = test_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    // Mark one block done so the save captures a lived-in day; apply must reset it to pending.
    run_ok(&context, ["ccplan", "done", "focus"]);

    // Nothing saved yet.
    assert_eq!(
        String::from_utf8(run_ok(&context, ["ccplan", "template", "list"]))
            .unwrap()
            .trim(),
        "no templates saved"
    );

    let saved =
        String::from_utf8(run_ok(&context, ["ccplan", "template", "save", "weekday"])).unwrap();
    assert!(
        saved.contains("saved template weekday from 2026-06-08"),
        "{saved}"
    );
    assert_eq!(
        String::from_utf8(run_ok(&context, ["ccplan", "template", "list"]))
            .unwrap()
            .trim(),
        "weekday"
    );

    // Instantiate onto a fresh date: blocks come back, all reset to pending, and triggers are armed.
    let applied = String::from_utf8(run_ok(
        &context,
        [
            "ccplan",
            "template",
            "apply",
            "weekday",
            "--date",
            "2026-06-09",
        ],
    ))
    .unwrap();
    assert!(
        applied.contains("applied template weekday to 2026-06-09"),
        "{applied}"
    );
    let instantiated = context
        .store
        .load_plan(&"2026-06-09".parse::<PlanDate>().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(instantiated.blocks.len(), 2);
    assert!(
        instantiated
            .blocks
            .iter()
            .all(|b| b.status == Status::Pending)
    );
    assert!(!context.scheduler.calls().is_empty());

    // An unsafe name is refused before it can touch the filesystem (path-traversal guard).
    assert!(matches!(
        run_err(&context, ["ccplan", "template", "save", "../escape"]),
        Error::Usage(message) if message.contains("template name must be")
    ));
    // Applying a template that does not exist is a not-found error.
    assert!(matches!(
        run_err(&context, ["ccplan", "template", "apply", "ghost"]),
        Error::NotFound(_)
    ));
}

fn fire_args(id: &str, event: &str, rev: &str, at: &str) -> Vec<String> {
    vec![
        "ccplan".to_owned(),
        "fire".to_owned(),
        "--date".to_owned(),
        "2026-06-08".to_owned(),
        "--id".to_owned(),
        id.to_owned(),
        "--event".to_owned(),
        event.to_owned(),
        "--rev".to_owned(),
        rev.to_owned(),
        "--at".to_owned(),
        at.to_owned(),
    ]
}

fn test_context_at(
    now: &str,
) -> (
    TempDir,
    Context<FixedClock, RecordingScheduler, RecordingNotifier>,
) {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let clock = FixedClock::new(now.parse::<Zoned>().unwrap());
    let context = Context::new(
        store,
        clock,
        RecordingScheduler::default(),
        RecordingNotifier::default(),
        Config::default(),
    );
    (temp, context)
}

fn failing_notifier_context_at(
    now: &str,
) -> (
    TempDir,
    Context<FixedClock, RecordingScheduler, FailingNotifier>,
) {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let clock = FixedClock::new(now.parse::<Zoned>().unwrap());
    let context = Context::new(
        store,
        clock,
        RecordingScheduler::default(),
        FailingNotifier,
        Config::default(),
    );
    (temp, context)
}

fn list_failing_scheduler_context_at(
    now: &str,
) -> (
    TempDir,
    Context<FixedClock, ListFailingScheduler, RecordingNotifier>,
) {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let clock = FixedClock::new(now.parse::<Zoned>().unwrap());
    let context = Context::new(
        store,
        clock,
        ListFailingScheduler,
        RecordingNotifier::default(),
        Config::default(),
    );
    (temp, context)
}

#[derive(Debug, Clone, Copy)]
struct FailingNotifier;

impl Notifier for FailingNotifier {
    fn check(&self) -> Result<(), NotifyError> {
        Err(NotifyError::Operation("no desktop bus".to_owned()))
    }

    fn notify(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Err(NotifyError::Operation("send failed".to_owned()))
    }
}

#[derive(Debug, Clone, Copy)]
struct ListFailingScheduler;

impl Scheduler for ListFailingScheduler {
    fn prepare(&self) -> Result<(), SchedulerError> {
        Ok(())
    }

    fn add(&self, _trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        Ok(())
    }

    fn remove(&self, _backend_id: &str) -> Result<(), SchedulerError> {
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        Err(SchedulerError::Operation("list failed".to_owned()))
    }
}

fn plan_toml() -> &'static str {
    r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus"
title = "Focus time"
start = "11:00"
end = "11:30"
notify = "5m"
tags = ["deep-work"]
status = "pending"

[[block]]
id = "lunch"
title = "Lunch"
start = "14:00"
duration = "30m"
status = "pending"
"#
}

fn plan() -> Plan {
    Plan::from_toml(plan_toml()).unwrap()
}

fn block_with(id: &str, title: &str, start: &str, span: Span) -> Block {
    Block {
        id: BlockId::new(id).unwrap(),
        title: title.to_owned(),
        start: start.parse::<ClockTime>().unwrap(),
        span,
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
        status: Status::Pending,
        run: None,
    }
}

fn date() -> PlanDate {
    "2026-06-08".parse().unwrap()
}

#[allow(dead_code)]
fn _timezone() -> TimeZoneName {
    "Asia/Kolkata".parse().unwrap()
}

#[allow(dead_code)]
fn _timestamp(value: &str) -> Timestamp {
    value.parse().unwrap()
}

#[allow(dead_code)]
fn _grace() -> SignedDuration {
    SignedDuration::from_secs(90)
}

#[test]
fn test_automation_refused_when_disabled() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context.config.automation.enabled = false;
    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(vec!["/bin/echo".to_owned()]).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);
    let err = run_err(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        matches!(err, Error::AutomationRefused(ref msg) if msg.contains("automation is disabled"))
    );
}

#[test]
fn test_automation_refused_when_not_absolute() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from("echo")];
    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(vec!["echo".to_owned()]).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);
    let err = run_err(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        matches!(err, Error::AutomationRefused(ref msg) if msg.contains("executable path is not absolute"))
    );
}

#[test]
fn test_automation_refused_when_not_allowlisted() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    // Both paths must be absolute on the host so the allowlist check (not the
    // absolute-path check) is the one that rejects: the run executable is absolute
    // but is not the (different, absolute) allowlisted one.
    let (run_exe, allowed_exe) = if cfg!(windows) {
        (
            "C:\\Windows\\System32\\cmd.exe",
            "C:\\Windows\\System32\\different.exe",
        )
    } else {
        ("/bin/echo", "/bin/different")
    };
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from(allowed_exe)];
    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(vec![run_exe.to_owned()]).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);
    let err = run_err(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        matches!(err, Error::AutomationRefused(ref msg) if msg.contains("executable not in allowlist"))
    );
}

#[test]
#[cfg(unix)]
fn test_automation_refused_when_bad_permissions() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from("/bin/echo")];
    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(vec!["/bin/echo".to_owned()]).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    // Set permissions to world-writable (0o666)
    use std::os::unix::fs::PermissionsExt;
    let plan_path = context.store.plan_path(&run_plan.date);
    let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
    perms.set_mode(0o666);
    std::fs::set_permissions(&plan_path, perms).unwrap();

    let rev = focus_rev(&context);
    let err = run_err(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );
    assert!(
        matches!(err, Error::AutomationRefused(ref msg) if msg.contains("plan file is group- or world-writable"))
    );
}

#[test]
fn test_automation_runs_and_kills_on_timeout() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    let exe = if cfg!(windows) {
        "C:\\Windows\\System32\\ping.exe"
    } else {
        "/bin/sleep"
    };
    let run_argv = if cfg!(windows) {
        vec![
            exe.to_owned(),
            "-n".to_owned(),
            "10".to_owned(),
            "127.0.0.1".to_owned(),
        ]
    } else {
        vec![exe.to_owned(), "10".to_owned()]
    };
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from(exe)];
    context.config.automation.timeout = ccplan::model::DurationSpec::from_seconds(1).unwrap();

    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(run_argv).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);
    run_ok(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );

    let log_content = std::fs::read_to_string(context.store.fire_log_path()).unwrap();
    assert!(
        log_content.contains("outcome=timeout"),
        "log should contain timeout: {}",
        log_content
    );
}

#[test]
fn test_automation_dry_run_prints_command() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    let exe = if cfg!(windows) {
        "C:\\Windows\\System32\\cmd.exe"
    } else {
        "/bin/echo"
    };
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from(exe)];

    let mut run_plan = plan();
    let run_argv = if cfg!(windows) {
        vec![
            exe.to_owned(),
            "/c".to_owned(),
            "echo".to_owned(),
            "hello".to_owned(),
        ]
    } else {
        vec![exe.to_owned(), "hello".to_owned()]
    };
    run_plan.blocks[0].run = Some(Run::new(run_argv).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);

    let mut args = fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z");
    args.push("--dry-run".to_owned());

    let output = run_ok(&context, args);
    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("dry-run:"));
    assert!(output_str.contains("would run command"));

    // --dry-run is read-only: nothing runs, nothing is logged, the block is not activated.
    assert!(
        !context.store.fire_log_path().exists(),
        "dry-run must not write a fire-log entry"
    );
    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].status, Status::Pending);
    assert!(context.notifier.notifications().is_empty());
}

#[test]
fn test_fire_dry_run_is_read_only_for_plain_activation() {
    let (_temp, context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&context);

    let mut args = fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z");
    args.push("--dry-run".to_owned());

    let output = run_ok(&context, args);
    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("dry-run: 2026-06-08 focus start ->"));
    assert!(output_str.contains("Activate"));

    // No side effects: the block stays pending, nothing is notified, no fire-log entry is written.
    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(stored.blocks[0].status, Status::Pending);
    assert!(context.notifier.notifications().is_empty());
    assert!(!context.store.fire_log_path().exists());
}

#[test]
fn test_fire_dry_run_leaves_ledger_untouched_so_real_fire_still_fires() {
    // The core guarantee of read-only `--dry-run`: it must NOT write the at-most-once ledger.
    // The status/notify/log assertions above would still pass even if the ledger were wrongly
    // recorded — the regression would only surface as a real fire afterward being silently
    // swallowed as AlreadyFired. So fire a dry run, then a real fire with identical coordinates,
    // and assert the real fire actually activates, notifies, and logs.
    let (_temp, context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&context);

    let mut dry = fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z");
    dry.push("--dry-run".to_owned());
    run_ok(&context, dry);

    // Same coordinates, no --dry-run: the real fire must not see a recorded ledger entry.
    run_ok(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );

    let stored = context.store.load_plan(&date()).unwrap().unwrap();
    assert_eq!(
        stored.blocks[0].status,
        Status::Active,
        "the real fire after a dry-run must still activate the block"
    );
    assert_eq!(context.notifier.notifications().len(), 1);
    let logged = std::fs::read_to_string(context.store.fire_log_path()).unwrap();
    assert!(logged.contains("\"id\":\"focus\""));
    assert!(logged.contains("\"event\":\"start\""));
}

#[test]
fn fire_notification_body_omits_the_slug_id() {
    // The notification title carries the human block name; the body must not repeat the machine
    // slug `id` (it used to render "focus at 11:00", which read as duplicate/robotic spam next to
    // the title). The body is just the start time now.
    let (_temp, context) = test_context_at("2026-06-08T10:55:00+05:30[Asia/Kolkata]");
    context
        .store
        .set_plan(&plan(), HistoryPolicy::Preserve)
        .unwrap();
    let rev = focus_rev(&context);

    run_ok(
        &context,
        fire_args("focus", "notify", rev.as_str(), "2026-06-08T05:25:00Z"),
    );

    let notifications = context.notifier.notifications();
    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    assert_eq!(notification.title, "Focus time");
    assert_eq!(notification.body, "at 11:00");
    assert!(
        !notification.body.contains("focus"),
        "body must not contain the slug id, got {:?}",
        notification.body
    );
}

#[test]
fn test_automation_truncates_large_output() {
    let (_temp, mut context) = test_context_at("2026-06-08T11:00:00+05:30[Asia/Kolkata]");
    let exe = if cfg!(windows) {
        "C:\\Windows\\System32\\cmd.exe"
    } else {
        "/usr/bin/seq"
    };
    let run_argv = if cfg!(windows) {
        vec![
            exe.to_owned(),
            "/c".to_owned(),
            "for /L %i in (1,1,600) do @echo aaaaaaaaaa".to_owned(),
        ]
    } else {
        vec![exe.to_owned(), "1".to_owned(), "2000".to_owned()]
    };
    context.config.automation.enabled = true;
    context.config.automation.allowed_executables = vec![std::path::PathBuf::from(exe)];
    context.config.automation.timeout = ccplan::model::DurationSpec::from_seconds(5).unwrap();

    let mut run_plan = plan();
    run_plan.blocks[0].run = Some(Run::new(run_argv).unwrap());
    context
        .store
        .set_plan(&run_plan, HistoryPolicy::Preserve)
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let plan_path = context.store.plan_path(&run_plan.date);
        let mut perms = std::fs::metadata(&plan_path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&plan_path, perms).unwrap();
    }

    let rev = focus_rev(&context);
    run_ok(
        &context,
        fire_args("focus", "start", rev.as_str(), "2026-06-08T05:30:00Z"),
    );

    let log_content = std::fs::read_to_string(context.store.fire_log_path()).unwrap();
    assert!(log_content.contains("outcome=success"));
    assert!(log_content.contains("stdout="));
}
