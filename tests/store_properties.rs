use ccplan::{
    lifecycle::Event,
    model::{Block, BlockId, ClockTime, DurationSpec, Lead, PlanDate, Span, Status},
    store::{FiredEventKey, FiredStatus, Store},
};
use jiff::Timestamp;
use proptest::prelude::*;

proptest! {
    #[test]
    fn fired_ledger_check_and_set_is_idempotent(id in "[a-z][a-z0-9-]{0,12}") {
        let temp = assert_fs::TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let key = fired_key(&id);

        prop_assert_eq!(
            store.check_and_set_fired(key.clone()).unwrap(),
            FiredStatus::Recorded
        );
        prop_assert_eq!(
            store.check_and_set_fired(key).unwrap(),
            FiredStatus::AlreadyFired
        );
    }
}

fn fired_key(id: &str) -> FiredEventKey {
    let block = Block {
        id: BlockId::new(id).unwrap(),
        title: "Generated".to_owned(),
        start: ClockTime::from_minutes_since_midnight(9 * 60).unwrap(),
        span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
        notify: Lead::from_seconds(0).unwrap(),
        tags: Vec::new(),
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

    FiredEventKey {
        date: "2026-06-08".parse::<PlanDate>().unwrap(),
        block_id: block.id.clone(),
        event: Event::Start,
        rev: block.schedule_rev(),
        scheduled_at: "2026-06-08T13:00:00Z".parse::<Timestamp>().unwrap(),
        attempt: 0,
        agent: None,
    }
}
