use ccplan::{
    model::{ClockTime, PlanDate, TimeZoneName},
    time::resolve,
};
use jiff::Timestamp;

#[test]
fn resolves_normal_wall_clock_time_to_timestamp() {
    let date = date("2026-06-08");
    let timezone = timezone("Asia/Kolkata");

    let resolved = resolve(&date, &timezone, wallclock("11:00")).expect("time should resolve");

    assert_eq!(resolved, timestamp("2026-06-08T05:30:00Z"));
}

#[test]
fn resolves_spring_forward_gap_with_compatible_strategy() {
    let date = date("2024-03-10");
    let timezone = timezone("America/New_York");

    let resolved = resolve(&date, &timezone, wallclock("02:30")).expect("gap should resolve");

    assert_eq!(resolved, timestamp("2024-03-10T07:30:00Z"));
}

#[test]
fn resolves_fall_back_fold_with_compatible_strategy() {
    let date = date("2024-11-03");
    let timezone = timezone("America/New_York");

    let resolved = resolve(&date, &timezone, wallclock("01:30")).expect("fold should resolve");

    assert_eq!(resolved, timestamp("2024-11-03T05:30:00Z"));
}

#[cfg(feature = "test-fakes")]
#[test]
fn fixed_clock_returns_the_injected_zoned_time() {
    use ccplan::time::{Clock, FixedClock};
    use jiff::Zoned;

    let now = "2026-06-08T11:00:00+05:30[Asia/Kolkata]"
        .parse::<Zoned>()
        .expect("fixture zoned time should parse");
    let clock = FixedClock::new(now.clone());

    assert_eq!(clock.now(), now);
}

fn date(value: &str) -> PlanDate {
    value.parse().expect("fixture date should parse")
}

fn timezone(value: &str) -> TimeZoneName {
    value.parse().expect("fixture time zone should parse")
}

fn wallclock(value: &str) -> ClockTime {
    value.parse().expect("fixture wall clock should parse")
}

fn timestamp(value: &str) -> Timestamp {
    value.parse().expect("fixture timestamp should parse")
}
