use assert_fs::TempDir;
use ccplan::{
    lifecycle::Event,
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName,
    },
    store::{FiredEventKey, FiredStatus, HistoryPolicy, Store, StoreError, TriggerRecord},
};
use jiff::Timestamp;

#[test]
fn store_round_trips_plan_under_injected_base_dir() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let plan = plan_with(vec![block("focus", Status::Pending)]);

    let stored = store
        .set_plan(&plan, HistoryPolicy::Preserve)
        .expect("plan should store");
    let loaded = store
        .load_plan(&plan.date)
        .expect("plan should load")
        .expect("plan should exist");

    assert_eq!(stored, plan);
    assert_eq!(loaded, plan);
}

#[test]
fn load_plan_rejects_invalid_hand_edited_file() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let date = date();
    let path = store.plan_path(&date);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "date = 'not-a-date'\ntimezone = 'America/New_York'\n",
    )
    .unwrap();

    let err = store
        .load_plan(&date)
        .expect_err("invalid hand edit should be rejected");

    assert_eq!(err.exit_code(), 2);
    assert!(matches!(err, StoreError::Plan(_)));
}

#[test]
fn stale_temp_file_does_not_replace_real_plan() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let original = plan_with(vec![block("focus", Status::Pending)]);
    store
        .set_plan(&original, HistoryPolicy::Preserve)
        .expect("original plan should store");
    let temp_path = store
        .plan_path(&original.date)
        .with_extension("toml.tmp-left");
    std::fs::write(&temp_path, "not the plan").unwrap();

    let loaded = store
        .load_plan(&original.date)
        .expect("real plan should still load")
        .expect("real plan should exist");

    assert_eq!(loaded, original);
    assert_eq!(std::fs::read_to_string(temp_path).unwrap(), "not the plan");
}

#[test]
fn set_plan_reports_io_error_when_plan_parent_is_blocked_by_file() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    std::fs::write(temp.path().join("data"), "blocks data dir").unwrap();

    let err = store
        .set_plan(
            &plan_with(vec![block("focus", Status::Pending)]),
            HistoryPolicy::Override,
        )
        .expect_err("file blocking data directory should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn second_lock_attempt_fails_cleanly() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let _guard = store.try_lock().expect("first lock should succeed");

    let err = store
        .set_plan(
            &plan_with(vec![block("focus", Status::Pending)]),
            HistoryPolicy::Preserve,
        )
        .expect_err("second lock should fail");

    assert!(matches!(err, StoreError::Locked));
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn lock_path_directory_reports_io_error() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let lock_path = temp.path().join("state").join("ccplan").join("store.lock");
    std::fs::create_dir_all(&lock_path).unwrap();

    let err = store
        .try_lock()
        .expect_err("directory lock path should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn set_plan_retains_terminal_blocks_omitted_from_incoming_plan() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![
        block("finished", Status::Done),
        block("old-pending", Status::Pending),
    ]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");
    let incoming = plan_with(vec![block("new-pending", Status::Pending)]);

    let merged = store
        .set_plan(&incoming, HistoryPolicy::Preserve)
        .expect("merge should preserve terminal history");

    assert_eq!(
        block_ids(&merged),
        vec!["new-pending".to_owned(), "finished".to_owned()]
    );
    assert_eq!(merged.blocks[1].status, Status::Done);
}

#[test]
fn set_plan_rejects_terminal_id_reuse_without_override() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![block("finished", Status::Done)]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");
    let incoming = plan_with(vec![block("finished", Status::Pending)]);

    let err = store
        .set_plan(&incoming, HistoryPolicy::Preserve)
        .expect_err("terminal id reuse should fail");

    assert_eq!(err.exit_code(), 6);
    assert!(matches!(err, StoreError::TerminalHistory { id } if id.as_str() == "finished"));
}

#[test]
fn set_plan_allows_identical_terminal_block_in_incoming_plan() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![block("finished", Status::Done)]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");

    let stored = store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("identical terminal history should be accepted");

    assert_eq!(stored, existing);
}

#[test]
fn set_plan_rejects_timezone_change_that_would_reinterpret_terminal_history() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![block("finished", Status::Done)]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");
    let incoming = plan_with_timezone("UTC", vec![block("new-pending", Status::Pending)]);

    let err = store
        .set_plan(&incoming, HistoryPolicy::Preserve)
        .expect_err("timezone change should not reinterpret terminal history");

    assert_eq!(err.exit_code(), 6);
    assert!(matches!(err, StoreError::TerminalHistory { id } if id.as_str() == "finished"));
}

#[test]
fn set_plan_allows_timezone_change_without_terminal_history() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![block("pending", Status::Pending)]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");
    let incoming = plan_with_timezone("UTC", vec![block("replacement", Status::Pending)]);

    let stored = store
        .set_plan(&incoming, HistoryPolicy::Preserve)
        .expect("timezone change is safe before terminal history exists");

    assert_eq!(stored, incoming);
}

#[test]
fn override_history_allows_terminal_id_reuse() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let existing = plan_with(vec![block("finished", Status::Done)]);
    store
        .set_plan(&existing, HistoryPolicy::Preserve)
        .expect("existing plan should store");
    let incoming = plan_with(vec![block("finished", Status::Pending)]);

    let stored = store
        .set_plan(&incoming, HistoryPolicy::Override)
        .expect("override should replace terminal history");

    assert_eq!(stored, incoming);
}

#[test]
fn archive_moves_plan_and_purge_removes_archived_plan() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let plan = plan_with(vec![block("focus", Status::Pending)]);
    store
        .set_plan(&plan, HistoryPolicy::Preserve)
        .expect("plan should store");

    assert!(store.archive(&plan.date).expect("archive should succeed"));
    assert!(store.load_plan(&plan.date).unwrap().is_none());
    assert!(store.archive_path(&plan.date).exists());

    assert!(
        store
            .purge(&plan.date)
            .expect("purge should remove archive")
    );
    assert!(!store.archive_path(&plan.date).exists());
}

#[test]
fn purge_reports_io_error_when_plan_path_is_directory() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    std::fs::create_dir_all(store.plan_path(&date())).unwrap();

    let err = store
        .purge(&date())
        .expect_err("directory at plan path should fail purge");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn archive_returns_false_when_plan_is_missing() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    assert!(!store.archive(&date()).expect("missing archive is a no-op"));
}

#[test]
fn archive_replaces_existing_archive_file() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let first = plan_with(vec![block("first", Status::Pending)]);
    let second = plan_with(vec![block("second", Status::Pending)]);
    store
        .set_plan(&first, HistoryPolicy::Preserve)
        .expect("first plan should store");
    assert!(store.archive(&first.date).unwrap());
    store
        .set_plan(&second, HistoryPolicy::Preserve)
        .expect("second plan should store");

    assert!(store.archive(&second.date).unwrap());
    let archived = Plan::from_toml(&std::fs::read_to_string(store.archive_path(&date())).unwrap())
        .expect("archive should stay a valid plan");

    assert_eq!(archived, second);
}

#[test]
fn archive_and_purge_prune_fired_keys_for_their_date() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    let today = fired_key("focus");
    let other = FiredEventKey {
        date: "2026-06-09".parse().unwrap(),
        ..fired_key("focus")
    };
    store.check_and_set_fired(today.clone()).unwrap();
    store.check_and_set_fired(other.clone()).unwrap();

    // Archiving the day retires its fired keys, but leaves other days intact.
    let plan = plan_with(vec![block("focus", Status::Pending)]);
    store.set_plan(&plan, HistoryPolicy::Preserve).unwrap();
    assert!(store.archive(&plan.date).unwrap());
    assert_eq!(
        store.check_and_set_fired(today.clone()).unwrap(),
        FiredStatus::Recorded,
        "archive should prune today's fired keys"
    );
    assert_eq!(
        store.check_and_set_fired(other.clone()).unwrap(),
        FiredStatus::AlreadyFired,
        "archive must not touch other days' fired keys"
    );

    // Purge likewise prunes its date's fired keys.
    store.set_plan(&plan, HistoryPolicy::Preserve).unwrap();
    assert!(store.purge(&plan.date).unwrap());
    assert_eq!(
        store.check_and_set_fired(today).unwrap(),
        FiredStatus::Recorded,
        "purge should prune today's fired keys"
    );
    assert_eq!(
        store.check_and_set_fired(other).unwrap(),
        FiredStatus::AlreadyFired,
        "purge must not touch other days' fired keys"
    );
}

#[test]
fn load_plan_reports_io_error_when_plan_path_is_directory() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    std::fs::create_dir_all(store.plan_path(&date())).unwrap();

    let err = store
        .load_plan(&date())
        .expect_err("directory at plan path should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn fired_ledger_check_and_set_is_durable() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let key = fired_key("focus");

    assert_eq!(
        store.check_and_set_fired(key.clone()).unwrap(),
        FiredStatus::Recorded
    );
    assert_eq!(
        store.check_and_set_fired(key.clone()).unwrap(),
        FiredStatus::AlreadyFired
    );

    let reloaded_store = Store::new(temp.path());
    assert_eq!(
        reloaded_store.check_and_set_fired(key).unwrap(),
        FiredStatus::AlreadyFired
    );
}

#[test]
fn invalid_fired_ledger_json_is_rejected() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let fired_path = temp.path().join("state").join("ccplan").join("fired.json");
    std::fs::create_dir_all(fired_path.parent().unwrap()).unwrap();
    std::fs::write(&fired_path, "{not-json").unwrap();

    let err = store
        .check_and_set_fired(fired_key("focus"))
        .expect_err("invalid JSON should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Json { .. }));
}

#[test]
fn fired_ledger_path_directory_reports_io_error() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let fired_path = temp.path().join("state").join("ccplan").join("fired.json");
    std::fs::create_dir_all(&fired_path).unwrap();

    let err = store
        .check_and_set_fired(fired_key("focus"))
        .expect_err("directory at fired ledger path should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn invalid_trigger_ledger_json_is_rejected() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let triggers_path = temp
        .path()
        .join("state")
        .join("ccplan")
        .join("triggers.json");
    std::fs::create_dir_all(triggers_path.parent().unwrap()).unwrap();
    std::fs::write(&triggers_path, "{not-json").unwrap();

    let err = store
        .list_triggers()
        .expect_err("invalid trigger JSON should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Json { .. }));
}

#[test]
fn trigger_ledger_path_directory_reports_io_error() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let triggers_path = temp
        .path()
        .join("state")
        .join("ccplan")
        .join("triggers.json");
    std::fs::create_dir_all(&triggers_path).unwrap();

    let err = store
        .list_triggers()
        .expect_err("directory at trigger ledger path should fail");

    assert_eq!(err.exit_code(), 1);
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn trigger_records_can_be_recorded_listed_and_removed() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let first = trigger("ccplan-first", "focus");
    let second = trigger("ccplan-second", "sync");

    store.record_trigger(first.clone()).unwrap();
    store.record_trigger(second.clone()).unwrap();
    assert_eq!(store.list_triggers().unwrap(), vec![first.clone(), second]);

    assert!(store.remove_trigger("ccplan-first").unwrap());
    assert_eq!(
        store.list_triggers().unwrap(),
        vec![trigger("ccplan-second", "sync")]
    );
    assert!(!store.remove_trigger("missing").unwrap());
}

#[test]
fn store_exposes_expected_config_path_under_injected_base() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    assert_eq!(
        store.config_path(),
        temp.path()
            .join("config")
            .join("ccplan")
            .join("config.toml")
    );
    assert_eq!(
        store.fire_log_path(),
        temp.path()
            .join("data")
            .join("ccplan")
            .join("log")
            .join("fire.log")
    );
}

fn plan_with(blocks: Vec<Block>) -> Plan {
    plan_with_timezone("America/New_York", blocks)
}

fn plan_with_timezone(timezone: &str, blocks: Vec<Block>) -> Plan {
    Plan {
        date: date(),
        timezone: timezone.parse::<TimeZoneName>().unwrap(),
        blocks,
    }
}

fn block(id: &str, status: Status) -> Block {
    Block {
        id: BlockId::new(id).unwrap(),
        title: format!("Block {id}"),
        start: "09:00".parse::<ClockTime>().unwrap(),
        span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
        status,
        run: Some(Run::new(vec!["/bin/echo".to_owned()]).unwrap()),
    }
}

fn block_ids(plan: &Plan) -> Vec<String> {
    plan.blocks
        .iter()
        .map(|block| block.id.as_str().to_owned())
        .collect()
}

fn fired_key(id: &str) -> FiredEventKey {
    let block = block(id, Status::Pending);
    FiredEventKey {
        date: date(),
        block_id: block.id.clone(),
        event: Event::Start,
        rev: block.schedule_rev(),
        scheduled_at: timestamp(),
    }
}

fn trigger(backend_id: &str, id: &str) -> TriggerRecord {
    let key = fired_key(id);
    TriggerRecord {
        backend_id: backend_id.to_owned(),
        date: key.date,
        block_id: key.block_id,
        event: key.event,
        rev: key.rev,
        scheduled_at: key.scheduled_at,
    }
}

fn date() -> PlanDate {
    "2026-06-08".parse().unwrap()
}

fn timestamp() -> Timestamp {
    "2026-06-08T13:00:00Z".parse().unwrap()
}
