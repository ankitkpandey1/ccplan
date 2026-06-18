use ccplan::{
    lifecycle::{EndBehavior, Event, FireDecision, LifecyclePolicy, decide_fire},
    model::{Approval, Block, BlockId, ClockTime, DurationSpec, Lead, Run, Span, Status},
};
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;

proptest! {
    #[test]
    fn terminal_blocks_never_transition_out(
        status in terminal_status_strategy(),
        event in event_strategy(),
        now_offset in -120_i64..180,
        end_behavior in end_behavior_strategy(),
    ) {
        let block = block_with(status, false);
        let decision = decide_fire(
            &block,
            event,
            target(),
            at_target_plus(now_offset),
            LifecyclePolicy::new(SignedDuration::from_secs(60), end_behavior),
        );

        prop_assert_eq!(decision, FireDecision::NoOp);
    }

    #[test]
    fn decide_fire_is_pure_over_its_inputs(
        status in status_strategy(),
        event in event_strategy(),
        has_run in any::<bool>(),
        now_offset in -120_i64..180,
        grace_seconds in 0_i64..180,
        end_behavior in end_behavior_strategy(),
    ) {
        let block = block_with(status, has_run);
        let policy = LifecyclePolicy::new(SignedDuration::from_secs(grace_seconds), end_behavior);
        let now = at_target_plus(now_offset);

        let first = decide_fire(&block, event, target(), now, policy);
        let second = decide_fire(&block, event, target(), now, policy);

        prop_assert_eq!(first, second);
    }
}

fn event_strategy() -> impl Strategy<Value = Event> {
    prop_oneof![Just(Event::Notify), Just(Event::Start), Just(Event::End)]
}

fn end_behavior_strategy() -> impl Strategy<Value = EndBehavior> {
    prop_oneof![Just(EndBehavior::AutoDone), Just(EndBehavior::Expire)]
}

fn status_strategy() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Pending),
        Just(Status::Active),
        terminal_status_strategy(),
    ]
}

fn terminal_status_strategy() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Done),
        Just(Status::Skipped),
        Just(Status::Missed),
        Just(Status::Expired),
    ]
}

fn block_with(status: Status, has_run: bool) -> Block {
    let run = has_run.then(|| Run::new(vec!["/bin/echo".to_owned()]).unwrap());
    let approval = run.as_ref().map(|_| Approval::Approved);
    Block {
        id: BlockId::new("focus").unwrap(),
        title: "Focus".to_owned(),
        start: ClockTime::from_minutes_since_midnight(9 * 60).unwrap(),
        span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
        status,
        run,
        recurrence: None,
        origin: None,
        after: vec![],
        on_success: vec![],
        on_failure: vec![],
        on_missed: vec![],
        retry: None,
        expect_by: None,
        approval,
        when: None,
        agent: None,
    }
}

fn target() -> Timestamp {
    "2026-06-08T10:00:00Z"
        .parse()
        .expect("fixture timestamp should parse")
}

fn at_target_plus(seconds: i64) -> Timestamp {
    target()
        .checked_add(SignedDuration::from_secs(seconds))
        .expect("fixture timestamp should be in range")
}
