use ccplan::model::{
    Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status, TimeZoneName,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn toml_round_trip_preserves_valid_plans(plan in plan_strategy()) {
        let toml = plan.to_toml().expect("valid generated plan should serialize");
        let reparsed = Plan::from_toml(&toml).expect("serialized plan should parse");

        prop_assert_eq!(reparsed, plan);
    }

    #[test]
    fn plan_schedule_revs_are_stable_under_block_reordering(plan in plan_strategy()) {
        let mut reordered = plan.clone();
        reordered.blocks.reverse();

        prop_assert_eq!(reordered.schedule_revs(), plan.schedule_revs());
    }

    #[test]
    fn lifecycle_and_content_edits_do_not_change_schedule_revs(plan in plan_strategy()) {
        let mut edited = plan.clone();
        for block in &mut edited.blocks {
            block.title.push_str(" edited");
            block.status = Status::Done;
            block.run = Some(Run::new(vec!["/bin/echo".to_owned(), "fresh".to_owned()]).unwrap());
        }

        prop_assert_eq!(edited.schedule_revs(), plan.schedule_revs());
    }
}

fn plan_strategy() -> impl Strategy<Value = Plan> {
    prop::collection::vec(block_strategy(), 0..8).prop_map(|mut blocks| {
        for (index, block) in blocks.iter_mut().enumerate() {
            block.id = BlockId::new(format!("block-{index}")).unwrap();
        }

        Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
            blocks,
        }
    })
}

fn block_strategy() -> impl Strategy<Value = Block> {
    (
        "[a-z][a-z0-9-]{0,8}",
        "[A-Za-z0-9 ][A-Za-z0-9 -]{0,24}",
        0_u16..1_320,
        1_u16..120,
        0_u32..600,
        prop::collection::vec("[a-z][a-z0-9-]{0,8}", 0..4),
        status_strategy(),
        prop::option::of(prop::collection::vec("[a-z0-9/_-]{1,12}", 1..4)),
    )
        .prop_map(
            |(id, title, start_minute, duration_minutes, notify_seconds, tags, status, run)| {
                Block {
                    id: BlockId::new(id).unwrap(),
                    title,
                    start: ClockTime::from_minutes_since_midnight(start_minute).unwrap(),
                    span: Span::Duration(
                        DurationSpec::from_seconds(u32::from(duration_minutes) * 60).unwrap(),
                    ),
                    notify: Lead::from_seconds(notify_seconds).unwrap(),
                    tags,
                    status,
                    run: run.map(|argv| Run::new(argv).unwrap()),
                }
            },
        )
}

fn status_strategy() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Pending),
        Just(Status::Active),
        Just(Status::Done),
        Just(Status::Skipped),
        Just(Status::Missed),
        Just(Status::Expired),
    ]
}
