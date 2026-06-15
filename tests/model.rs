use ccplan::model::{
    Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, PlanError, Run, ScheduleRev,
    Span, Status, TimeZoneName, ValidationError,
};

const PLAN_TOML: &str = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
end = "11:30"
notify = "0m"
tags = ["deep-work"]
status = "pending"

[[block]]
id = "sync-1"
title = "Agentic sync-up"
start = "11:30"
duration = "30m"
notify = "2m"
run = ["/usr/local/bin/sync.sh", "--fast"]
status = "pending"
"#;

#[test]
fn parses_and_writes_design_toml_schema() {
    let plan = Plan::from_toml(PLAN_TOML).expect("fixture should parse");

    assert_eq!(plan.date.to_string(), "2026-06-08");
    assert_eq!(plan.timezone.as_str(), "Asia/Kolkata");
    assert_eq!(plan.blocks.len(), 2);
    assert_eq!(plan.blocks[0].id.as_str(), "focus-1");
    assert_eq!(plan.blocks[0].status, Status::Pending);
    assert_eq!(
        plan.blocks[1].run.as_ref().map(Run::as_slice),
        Some(["/usr/local/bin/sync.sh".to_owned(), "--fast".to_owned()].as_slice())
    );

    let written = plan.to_toml().expect("fixture should serialize");

    assert!(written.contains("[[block]]"));
    assert!(!written.contains("[[blocks]]"));
    assert_eq!(
        Plan::from_toml(&written).expect("written TOML should parse"),
        plan
    );
}

#[test]
fn duration_and_clock_parsers_accept_required_forms() {
    assert_eq!("30m".parse::<DurationSpec>().unwrap().as_seconds(), 1_800);
    assert_eq!("90s".parse::<DurationSpec>().unwrap().as_seconds(), 90);
    assert_eq!("90s".parse::<DurationSpec>().unwrap().to_string(), "1m30s");
    assert_eq!("1h30m".parse::<DurationSpec>().unwrap().as_seconds(), 5_400);
    assert_eq!(
        "1h30m".parse::<DurationSpec>().unwrap().to_string(),
        "1h30m"
    );
    assert_eq!("0m".parse::<Lead>().unwrap().as_seconds(), 0);
    assert_eq!(
        "11:00"
            .parse::<ClockTime>()
            .unwrap()
            .minutes_since_midnight(),
        660
    );
}

#[test]
fn field_parsers_reject_malformed_values() {
    assert!(BlockId::new("").is_err());
    assert!(BlockId::new("bad id").is_err());
    assert_eq!(BlockId::new("focus-1").unwrap().to_string(), "focus-1");

    assert!("2026-99-99".parse::<PlanDate>().is_err());
    assert!("Not/AZone".parse::<TimeZoneName>().is_err());
    assert_eq!(
        "america/NEW_YORK"
            .parse::<TimeZoneName>()
            .unwrap()
            .to_string(),
        "America/New_York"
    );

    assert!(ClockTime::from_minutes_since_midnight(1_440).is_err());
    assert!("1100".parse::<ClockTime>().is_err());
    assert!("1:00".parse::<ClockTime>().is_err());
    assert!("aa:00".parse::<ClockTime>().is_err());
    assert!("11:aa".parse::<ClockTime>().is_err());
    assert!("24:00".parse::<ClockTime>().is_err());
    assert_eq!("09:05".parse::<ClockTime>().unwrap().to_string(), "09:05");

    assert!(DurationSpec::from_seconds(0).is_err());
    assert!(DurationSpec::from_seconds(86_401).is_err());
    assert!(Lead::from_seconds(86_401).is_err());
    assert!(Run::new(Vec::new()).is_err());
    assert!("not-a-rev".parse::<ScheduleRev>().is_err());
    assert!("".parse::<DurationSpec>().is_err());
    assert!("10".parse::<DurationSpec>().is_err());
    assert!("m".parse::<DurationSpec>().is_err());
    assert!("1x".parse::<DurationSpec>().is_err());
    assert!("1m2h".parse::<DurationSpec>().is_err());
    assert!("4294967296s".parse::<DurationSpec>().is_err());
    assert!("4294967295h1s".parse::<DurationSpec>().is_err());
}

#[test]
fn unknown_fields_are_rejected_on_read() {
    let err = Plan::from_toml(
        r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"
unexpected = true
"#,
    )
    .expect_err("top-level unknown field should fail");

    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("unknown field"));

    let err = Plan::from_toml(
        r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
end = "11:30"
surprise = true
"#,
    )
    .expect_err("block unknown field should fail");

    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn validation_reports_duplicate_ids() {
    let mut plan = valid_plan();
    plan.blocks.push(valid_block("focus-1"));

    assert!(matches!(
        plan.validate(),
        Err(ValidationError::DuplicateId { id }) if id.as_str() == "focus-1"
    ));
}

#[test]
fn validation_requires_exactly_one_end_shape() {
    let missing = Plan::from_toml(
        r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
"#,
    );
    assert!(matches!(
        missing,
        Err(PlanError::Validation(ValidationError::MissingEndOrDuration { id }))
            if id.as_str() == "focus-1"
    ));

    let both = Plan::from_toml(
        r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
end = "11:30"
duration = "30m"
"#,
    );
    assert!(matches!(
        both,
        Err(PlanError::Validation(ValidationError::BothEndAndDuration { id }))
            if id.as_str() == "focus-1"
    ));
}

#[test]
fn validation_rejects_empty_run_argv() {
    let plan = Plan::from_toml(
        r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus-1"
title = "Focus time"
start = "11:00"
end = "11:30"
run = []
"#,
    );

    assert!(matches!(
        plan,
        Err(PlanError::Validation(ValidationError::EmptyRun { id }))
            if id.as_str() == "focus-1"
    ));
}

#[test]
fn validation_rejects_non_forward_timing() {
    let mut plan = valid_plan();
    plan.blocks[0].span = Span::End("10:59".parse().unwrap());

    assert!(matches!(
        plan.validate(),
        Err(ValidationError::EndNotAfterStart { id }) if id.as_str() == "focus-1"
    ));
}

#[test]
fn validation_rejects_duration_past_midnight() {
    let mut plan = valid_plan();
    plan.blocks[0].start = "23:30".parse().unwrap();
    plan.blocks[0].span = Span::Duration("1h".parse().unwrap());
    let err = plan
        .validate()
        .expect_err("duration crossing midnight should fail");

    assert_eq!(err.exit_code(), 2);
    assert!(matches!(
        err,
        ValidationError::EndPastDay { id } if id.as_str() == "focus-1"
    ));
}

#[test]
fn schedule_rev_excludes_lifecycle_and_content_fields() {
    let block = valid_block("focus-1");
    let rev = block.schedule_rev();
    let mut edited = block.clone();

    edited.title = "Different title".to_owned();
    edited.status = Status::Done;
    edited.tags.push("changed".to_owned());
    edited.run = Some(Run::new(vec!["/bin/echo".to_owned(), "fresh".to_owned()]).unwrap());

    assert_eq!(edited.schedule_rev(), rev);
}

#[test]
fn schedule_rev_changes_when_timing_changes() {
    let block = valid_block("focus-1");
    let rev = block.schedule_rev();

    let mut retimed = block.clone();
    retimed.start = "11:01".parse().unwrap();
    assert_ne!(retimed.schedule_rev(), rev);

    let mut renotified = block;
    renotified.notify = "5m".parse().unwrap();
    assert_ne!(renotified.schedule_rev(), rev);
}

#[test]
fn schedule_rev_treats_equivalent_end_and_duration_as_same_timing() {
    let end_block = valid_block("focus-1");
    let mut duration_block = end_block.clone();
    duration_block.span = Span::Duration("30m".parse().unwrap());

    assert_eq!(duration_block.schedule_rev(), end_block.schedule_rev());
}

#[test]
fn schedule_rev_is_displayable_even_for_incomplete_draft_blocks() {
    let block = valid_block("focus-1");
    let rev = block.schedule_rev();

    assert_eq!(rev.as_str().len(), 16);
    assert_eq!(rev.to_string(), rev.as_str());
}

fn valid_plan() -> Plan {
    Plan {
        date: "2026-06-08".parse::<PlanDate>().unwrap(),
        timezone: "Asia/Kolkata".parse::<TimeZoneName>().unwrap(),
        blocks: vec![valid_block("focus-1")],
    }
}

fn valid_block(id: &str) -> Block {
    Block {
        id: BlockId::new(id).unwrap(),
        title: "Focus time".to_owned(),
        start: "11:00".parse().unwrap(),
        span: Span::End("11:30".parse().unwrap()),
        notify: "0m".parse().unwrap(),
        tags: vec!["deep-work".to_owned()],
        status: Status::Pending,
        run: None,
    }
}
