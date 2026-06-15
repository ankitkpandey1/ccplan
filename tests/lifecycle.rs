use ccplan::{
    lifecycle::{
        EndBehavior, Event, FireDecision, LifecyclePolicy, StatusUpdate, decide_fire,
        reconcile_overdue,
    },
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName,
    },
};
use jiff::{SignedDuration, Timestamp};

#[test]
fn fire_notify_on_time_sends_notification() {
    let block = block_with(Status::Pending, None);

    let decision = decide_fire(
        &block,
        Event::Notify,
        target(),
        at_target_plus(60),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::Notify);
}

#[test]
fn event_parses_and_displays_cli_values() {
    assert_eq!("notify".parse::<Event>().unwrap(), Event::Notify);
    assert_eq!("start".parse::<Event>().unwrap(), Event::Start);
    assert_eq!("end".parse::<Event>().unwrap(), Event::End);
    assert_eq!(Event::Notify.to_string(), "notify");
    assert!(
        "bad"
            .parse::<Event>()
            .unwrap_err()
            .to_string()
            .contains("bad")
    );
}

#[test]
fn fire_notify_overdue_noops_without_status_change() {
    let block = block_with(Status::Pending, None);

    let decision = decide_fire(
        &block,
        Event::Notify,
        target(),
        at_target_plus(61),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::NoOp);
}

#[test]
fn fire_start_on_time_activates_without_run() {
    let block = block_with(Status::Pending, None);

    let decision = decide_fire(
        &block,
        Event::Start,
        target(),
        target(),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::Activate { run: false });
}

#[test]
fn fire_start_on_time_includes_run_when_block_has_run() {
    let block = block_with(
        Status::Pending,
        Some(Run::new(vec!["/bin/echo".to_owned(), "hello".to_owned()]).unwrap()),
    );

    let decision = decide_fire(
        &block,
        Event::Start,
        target(),
        target(),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::Activate { run: true });
}

#[test]
fn fire_start_overdue_pending_marks_missed() {
    let block = block_with(Status::Pending, None);

    let decision = decide_fire(
        &block,
        Event::Start,
        target(),
        at_target_plus(61),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::MarkMissed);
}

#[test]
fn fire_start_overdue_active_noops() {
    let block = block_with(Status::Active, None);

    let decision = decide_fire(
        &block,
        Event::Start,
        target(),
        at_target_plus(61),
        policy(EndBehavior::Expire),
    );

    assert_eq!(decision, FireDecision::NoOp);
}

#[test]
fn fire_end_active_closes_done_when_auto_done_is_enabled() {
    let block = block_with(Status::Active, None);

    let decision = decide_fire(
        &block,
        Event::End,
        target(),
        target(),
        policy(EndBehavior::AutoDone),
    );

    assert_eq!(
        decision,
        FireDecision::Close {
            status: Status::Done
        }
    );
}

#[test]
fn fire_end_active_closes_expired_when_auto_done_is_disabled() {
    let block = block_with(Status::Active, None);

    let decision = decide_fire(
        &block,
        Event::End,
        target(),
        at_target_plus(61),
        policy(EndBehavior::Expire),
    );

    assert_eq!(
        decision,
        FireDecision::Close {
            status: Status::Expired
        }
    );
}

#[test]
fn fire_end_terminal_noops() {
    for status in [
        Status::Done,
        Status::Skipped,
        Status::Missed,
        Status::Expired,
    ] {
        let block = block_with(status, None);

        let decision = decide_fire(
            &block,
            Event::End,
            target(),
            at_target_plus(61),
            policy(EndBehavior::AutoDone),
        );

        assert_eq!(decision, FireDecision::NoOp);
    }
}

#[test]
fn grace_boundary_is_on_time_and_next_second_is_overdue() {
    let block = block_with(Status::Pending, None);

    let inside = decide_fire(
        &block,
        Event::Start,
        target(),
        at_target_plus(60),
        policy(EndBehavior::Expire),
    );
    let outside = decide_fire(
        &block,
        Event::Start,
        target(),
        at_target_plus(61),
        policy(EndBehavior::Expire),
    );

    assert_eq!(inside, FireDecision::Activate { run: false });
    assert_eq!(outside, FireDecision::MarkMissed);
}

#[test]
fn reconcile_overdue_marks_pending_missed_and_active_expired() {
    let plan = plan_with(vec![
        block_with_id(
            "pending-overdue",
            Status::Pending,
            "09:00",
            Span::End(wallclock("09:30")),
        ),
        block_with_id(
            "active-overdue",
            Status::Active,
            "10:00",
            Span::End(wallclock("10:30")),
        ),
        block_with_id(
            "done-history",
            Status::Done,
            "11:00",
            Span::End(wallclock("11:30")),
        ),
    ]);

    let updates = reconcile_overdue(&plan, timestamp("2026-06-08T17:00:00Z"), grace())
        .expect("valid plan times should resolve");

    assert_eq!(
        updates,
        vec![
            StatusUpdate {
                id: block_id("active-overdue"),
                status: Status::Expired,
            },
            StatusUpdate {
                id: block_id("pending-overdue"),
                status: Status::Missed,
            },
        ]
    );
}

#[test]
fn reconcile_uses_end_grace_boundary_for_active_blocks() {
    let plan = plan_with(vec![block_with_id(
        "active",
        Status::Active,
        "10:00",
        Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
    )]);
    let end = timestamp("2026-06-08T14:30:00Z");

    let inside = reconcile_overdue(
        &plan,
        end.checked_add(SignedDuration::from_secs(60)).unwrap(),
        grace(),
    )
    .expect("valid plan times should resolve");
    let outside = reconcile_overdue(
        &plan,
        end.checked_add(SignedDuration::from_secs(61)).unwrap(),
        grace(),
    )
    .expect("valid plan times should resolve");

    assert!(inside.is_empty());
    assert_eq!(
        outside,
        vec![StatusUpdate {
            id: block_id("active"),
            status: Status::Expired,
        }]
    );
}

fn policy(end_behavior: EndBehavior) -> LifecyclePolicy {
    LifecyclePolicy::new(grace(), end_behavior)
}

fn grace() -> SignedDuration {
    SignedDuration::from_secs(60)
}

fn target() -> Timestamp {
    timestamp("2026-06-08T10:00:00Z")
}

fn at_target_plus(seconds: i64) -> Timestamp {
    target()
        .checked_add(SignedDuration::from_secs(seconds))
        .expect("fixture timestamp should be in range")
}

fn plan_with(blocks: Vec<Block>) -> Plan {
    Plan {
        date: "2026-06-08".parse::<PlanDate>().unwrap(),
        timezone: "America/New_York".parse::<TimeZoneName>().unwrap(),
        blocks,
    }
}

fn block_with(status: Status, run: Option<Run>) -> Block {
    let mut block = block_with_id("focus", status, "09:00", Span::End(wallclock("09:30")));
    block.run = run;
    block
}

fn block_with_id(id: &str, status: Status, start: &str, span: Span) -> Block {
    Block {
        id: block_id(id),
        title: "Focus".to_owned(),
        start: start.parse::<ClockTime>().unwrap(),
        span,
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
        status,
        run: None,
    }
}

fn block_id(value: &str) -> BlockId {
    BlockId::new(value).unwrap()
}

fn wallclock(value: &str) -> ClockTime {
    value.parse().unwrap()
}

fn timestamp(value: &str) -> Timestamp {
    value.parse().expect("fixture timestamp should parse")
}
