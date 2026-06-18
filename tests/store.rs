use assert_fs::TempDir;
use ccplan::{
    lifecycle::Event,
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Origin, Plan, PlanDate, RecurRule,
        Recurrence, RecurringRules, Run, Span, Status, TimeZoneName,
    },
    store::{
        FireRecord, FiredEventKey, FiredStatus, HistoryPolicy, Store, StoreError, TriggerKind,
        TriggerRecord, compute_gen_hash,
    },
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
        recurrence: None,
        origin: None,
        after: vec![],
        on_success: vec![],
        on_failure: vec![],
        on_missed: vec![],
        retry: None,
        expect_by: None,
        approval: Some(ccplan::model::Approval::Pending),
        when: None,
        agent: None,
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
        attempt: 0,
        agent: None,
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
        kind: TriggerKind::Fire,
        attempt: 0,
    }
}

fn date() -> PlanDate {
    "2026-06-08".parse().unwrap()
}

fn timestamp() -> Timestamp {
    "2026-06-08T13:00:00Z".parse().unwrap()
}

// ── M3: recurring rules, materialize, gen_hash, dead-man ────────────────────

fn recurring_block(id: &str) -> Block {
    Block {
        id: BlockId::new(id).unwrap(),
        title: format!("Recurring {id}"),
        start: "09:00".parse::<ClockTime>().unwrap(),
        span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
        status: Status::Pending,
        run: None,
        recurrence: Some(Recurrence {
            rule: RecurRule::Daily,
            anchor: "2026-06-08".parse().unwrap(),
            end: None,
        }),
        origin: None,
        after: vec![],
        on_success: vec![],
        on_failure: vec![],
        on_missed: vec![],
        retry: None,
        expect_by: None,
        approval: None,
        when: None,
        agent: None,
    }
}

#[test]
fn load_recurring_rules_returns_empty_when_file_missing() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let rules = store
        .load_recurring_rules()
        .expect("should succeed with missing file");
    assert!(rules.blocks.is_empty());
}

#[test]
fn save_and_load_recurring_rules_round_trips() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let mut rules = RecurringRules::default();
    rules.blocks.push(recurring_block("standup"));

    store.save_recurring_rules(&rules).expect("should save");
    let loaded = store.load_recurring_rules().expect("should load");
    assert_eq!(loaded.blocks.len(), 1);
    assert_eq!(loaded.blocks[0].id.as_str(), "standup");
}

#[test]
fn compute_gen_hash_is_deterministic() {
    let b = recurring_block("focus");
    assert_eq!(compute_gen_hash(&b), compute_gen_hash(&b));
}

#[test]
fn compute_gen_hash_changes_when_start_changes() {
    let b1 = recurring_block("focus");
    let mut b2 = b1.clone();
    b2.start = "10:00".parse().unwrap();
    assert_ne!(compute_gen_hash(&b1), compute_gen_hash(&b2));
}

#[test]
fn materialize_for_date_generates_occurrence_for_matching_rule() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let mut rules = RecurringRules::default();
    rules.blocks.push(recurring_block("standup"));
    store.save_recurring_rules(&rules).unwrap();

    let plan = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .expect("materialize should succeed");

    assert_eq!(plan.blocks.len(), 1);
    assert_eq!(plan.blocks[0].id.as_str(), "standup");
    assert!(plan.blocks[0].origin.is_some());
    assert!(plan.blocks[0].recurrence.is_none()); // occurrences are concrete
}

#[test]
fn materialize_skips_rules_that_do_not_match_date() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    // weekly:tue — date() is 2026-06-08 which is a Monday.
    let mut rule = recurring_block("tuesday-sync");
    rule.recurrence = Some(Recurrence {
        rule: RecurRule::Weekly(vec![ccplan::model::Weekday::Tuesday]),
        anchor: "2026-06-08".parse().unwrap(),
        end: None,
    });
    let mut rules = RecurringRules::default();
    rules.blocks.push(rule);
    store.save_recurring_rules(&rules).unwrap();

    let plan = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .expect("materialize should succeed");

    assert!(plan.blocks.is_empty());
}

#[test]
fn materialize_preserves_user_modified_generated_block() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let rule = recurring_block("standup");
    let mut rules = RecurringRules::default();
    rules.blocks.push(rule.clone());
    store.save_recurring_rules(&rules).unwrap();

    // Materialize once so the generated block exists.
    let plan = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .unwrap();
    store.set_plan(&plan, HistoryPolicy::Override).unwrap();

    // Now "user edits" by setting a wrong gen_hash on the stored block.
    let mut mutated = plan.clone();
    mutated.blocks[0].origin = Some(Origin {
        rule_id: BlockId::new("standup").unwrap(),
        gen_hash: "user-modified-hash".to_owned(),
    });
    store.set_plan(&mutated, HistoryPolicy::Override).unwrap();

    // Re-materialize: user-modified block should survive, not be replaced.
    let re_plan = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .unwrap();
    let kept = re_plan
        .blocks
        .iter()
        .find(|b| b.id.as_str() == "standup")
        .expect("user-modified block should be kept");
    assert_eq!(kept.origin.as_ref().unwrap().gen_hash, "user-modified-hash");
}

#[test]
fn materialize_returns_collision_error_for_hand_authored_id_matching_rule() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    // Save a recurring rule with id "standup".
    let mut rules = RecurringRules::default();
    rules.blocks.push(recurring_block("standup"));
    store.save_recurring_rules(&rules).unwrap();

    // Save a hand-authored plan (no origin) with id "standup".
    let hand_block = Block {
        id: BlockId::new("standup").unwrap(),
        title: "Hand standup".to_owned(),
        start: "10:00".parse::<ClockTime>().unwrap(),
        span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
        notify: Lead::from_seconds(0).unwrap(),
        tags: vec![],
        status: Status::Pending,
        run: None,
        recurrence: None,
        origin: None,
        after: vec![],
        on_success: vec![],
        on_failure: vec![],
        on_missed: vec![],
        retry: None,
        expect_by: None,
        approval: None,
        when: None,
        agent: None,
    };
    let hand_plan = Plan {
        date: date(),
        timezone: "UTC".parse().unwrap(),
        blocks: vec![hand_block],
    };
    store.set_plan(&hand_plan, HistoryPolicy::Override).unwrap();

    let err = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .expect_err("collision should be an error");
    assert!(matches!(err, StoreError::RecurrenceCollision { .. }));
    assert_eq!(err.exit_code(), 6);
}

#[test]
fn load_recurring_rules_returns_error_on_non_notfound_io() {
    // Creating a directory at the recurring.toml path causes IsADirectory error (not NotFound),
    // which exercises the Err(source) arm at store.rs line 108.
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let path = store.recurring_path();
    std::fs::create_dir_all(&path).unwrap(); // path is now a directory, not a file
    let err = store
        .load_recurring_rules()
        .expect_err("directory-as-file should return IO error");
    assert!(matches!(err, StoreError::Io { .. }));
}

#[test]
fn materialize_for_date_regenerates_untouched_generated_block() {
    // The second materialize_for_date finds an existing block with matching gen_hash
    // and skips it (line 191 continue), then regenerates it from rules.
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let rule = recurring_block("standup");
    let mut rules = RecurringRules::default();
    rules.blocks.push(rule.clone());
    store.save_recurring_rules(&rules).unwrap();

    // First pass: generate block and save to store.
    let plan1 = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .unwrap();
    store.set_plan(&plan1, HistoryPolicy::Override).unwrap();
    // plan1.blocks[0].origin.gen_hash == compute_gen_hash(&rule)

    // Second pass: existing block has matching gen_hash → skipped (line 191), regenerated.
    let plan2 = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .unwrap();
    assert_eq!(plan2.blocks.len(), 1);
    assert_eq!(plan2.blocks[0].id.as_str(), "standup");
    assert_eq!(
        plan2.blocks[0].origin.as_ref().unwrap().gen_hash,
        compute_gen_hash(&rule)
    );
}

#[test]
fn materialize_for_date_keeps_ordinary_hand_authored_block() {
    // An existing plan with a plain block (no origin, id not matching any rule) goes through
    // the else branch at store.rs lines 200-203 (always keep).
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());

    // Save a plan with an ordinary block (no recurring rules).
    let p = plan_with(vec![block("meeting", Status::Pending)]);
    store.set_plan(&p, HistoryPolicy::Preserve).unwrap();

    // Materialize with no rules: the ordinary block should be kept.
    let result = store
        .materialize_for_date(&date(), Lead::from_seconds(0).unwrap())
        .unwrap();
    assert_eq!(result.blocks.len(), 1);
    assert_eq!(result.blocks[0].id.as_str(), "meeting");
    assert!(result.blocks[0].origin.is_none());
}

#[test]
fn dead_man_check_skips_other_block_records_and_non_success_outcomes() {
    // Fire log contains a record for a different block (hits line 242 continue) and a record
    // for the target block with a non-success outcome (hits line 247, the } of the inner if).
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let block_id = BlockId::new("standup").unwrap();
    let other_id = BlockId::new("focus").unwrap();
    let fired_at: Timestamp = "2026-06-08T09:00:00Z".parse().unwrap();
    let now: Timestamp = "2026-06-08T14:00:00Z".parse().unwrap();

    std::fs::create_dir_all(store.fire_log_path().parent().unwrap()).unwrap();
    let records = [
        FireRecord {
            ts: fired_at,
            date: date(),
            id: other_id.clone(),
            event: ccplan::lifecycle::Event::Start,
            outcome: "notify".to_owned(),
            detail: String::new(),
            agent: None,
        },
        // This record's id matches but outcome is not "notify"/"run-ok" → line 247 } hit.
        FireRecord {
            ts: fired_at,
            date: date(),
            id: block_id.clone(),
            event: ccplan::lifecycle::Event::Start,
            outcome: "activate".to_owned(),
            detail: String::new(),
            agent: None,
        },
    ];
    let content: String = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap() + "\n")
        .collect();
    std::fs::write(store.fire_log_path(), content).unwrap();

    // No success record for "standup" → dead man tripped.
    let result = store.dead_man_check(&block_id, 3600, &now).unwrap();
    assert!(result, "no success record → dead man should trip");
}

#[test]
fn compute_gen_hash_uses_end_span_variant() {
    // Exercises the Span::End arm in compute_gen_hash (lines 804-807).
    let mut b = recurring_block("standup");
    b.span = Span::End("10:00".parse::<ClockTime>().unwrap());
    let h1 = compute_gen_hash(&b);
    let h2 = compute_gen_hash(&b);
    assert_eq!(h1, h2, "gen_hash must be deterministic for Span::End");
    // Differs from the Duration variant.
    let b_dur = recurring_block("standup");
    assert_ne!(
        h1,
        compute_gen_hash(&b_dur),
        "End and Duration hashes must differ"
    );
}

#[test]
fn dead_man_check_returns_true_when_no_fire_record_exists() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let now: Timestamp = "2026-06-08T14:00:00Z".parse().unwrap();
    let id = BlockId::new("standup").unwrap();
    let result = store.dead_man_check(&id, 3600, &now).unwrap();
    assert!(result, "no records → dead man should trip");
}

#[test]
fn dead_man_check_returns_false_when_recent_success() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let block_id = BlockId::new("standup").unwrap();
    let fired_at: Timestamp = "2026-06-08T09:00:00Z".parse().unwrap();
    let now: Timestamp = "2026-06-08T09:30:00Z".parse().unwrap(); // 30 min later

    // Write a fire record with notify outcome (success).
    std::fs::create_dir_all(store.fire_log_path().parent().unwrap()).unwrap();
    let record = FireRecord {
        ts: fired_at,
        date: date(),
        id: block_id.clone(),
        event: ccplan::lifecycle::Event::Start,
        outcome: "notify".to_owned(),
        detail: String::new(),
        agent: None,
    };
    let line = serde_json::to_string(&record).unwrap() + "\n";
    std::fs::write(store.fire_log_path(), line).unwrap();

    // expect_by = 3600s (1h), elapsed = 30m → NOT tripped.
    let result = store.dead_man_check(&block_id, 3600, &now).unwrap();
    assert!(!result, "recent success should not trip dead man");
}

#[test]
fn dead_man_check_returns_true_when_success_is_too_old() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path());
    let block_id = BlockId::new("standup").unwrap();
    let fired_at: Timestamp = "2026-06-08T07:00:00Z".parse().unwrap();
    let now: Timestamp = "2026-06-08T10:00:00Z".parse().unwrap(); // 3h later

    std::fs::create_dir_all(store.fire_log_path().parent().unwrap()).unwrap();
    let record = FireRecord {
        ts: fired_at,
        date: date(),
        id: block_id.clone(),
        event: ccplan::lifecycle::Event::Start,
        outcome: "notify".to_owned(),
        detail: String::new(),
        agent: None,
    };
    let line = serde_json::to_string(&record).unwrap() + "\n";
    std::fs::write(store.fire_log_path(), line).unwrap();

    // expect_by = 3600s (1h), elapsed = 3h → tripped.
    let result = store.dead_man_check(&block_id, 3600, &now).unwrap();
    assert!(result, "stale success should trip dead man");
}
