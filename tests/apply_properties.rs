use assert_fs::TempDir;
use ccplan::{
    cli::Cli,
    config::Config,
    context::{Context, RecordingNotifier, RecordingScheduler},
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Span, Status, TimeZoneName,
    },
    run_with_context,
    store::{HistoryPolicy, Store},
    time::FixedClock,
};
use clap::Parser;
use jiff::Zoned;
use proptest::prelude::*;

proptest! {
    #[test]
    fn apply_is_idempotent_for_future_blocks(start_minute in 660_u16..720, duration_minutes in 10_u32..90) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let context = Context::new(
            store,
            FixedClock::new("2026-06-08T10:00:00+05:30[Asia/Kolkata]".parse::<Zoned>().unwrap()),
            RecordingScheduler::default(),
            RecordingNotifier::default(),
            Config::default(),
        );
        let plan = Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
            blocks: vec![Block {
                id: BlockId::new("generated").unwrap(),
                title: "Generated".to_owned(),
                start: ClockTime::from_minutes_since_midnight(start_minute).unwrap(),
                span: Span::Duration(DurationSpec::from_seconds(duration_minutes * 60).unwrap()),
                notify: Lead::from_seconds(0).unwrap(),
                tags: Vec::new(),
                status: Status::Pending,
                run: None,
            }],
        };
        context.store.set_plan(&plan, HistoryPolicy::Preserve).unwrap();

        let cli = Cli::parse_from(["ccplan", "apply"]);
        run_with_context(cli, &mut Vec::new(), &context).unwrap();
        prop_assert!(!context.scheduler.calls().is_empty());
        context.scheduler.clear_calls();

        let cli = Cli::parse_from(["ccplan", "apply"]);
        run_with_context(cli, &mut Vec::new(), &context).unwrap();

        prop_assert!(context.scheduler.calls().is_empty());
    }
}
