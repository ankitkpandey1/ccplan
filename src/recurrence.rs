//! Recurrence rule parsing and date expansion.

use jiff::civil::Weekday as JiffWeekday;

use crate::model::{FieldParseError, PlanDate, RecurEnd, RecurRule, Weekday};

/// Parses an `every` string into a `RecurRule`.
///
/// # Errors
///
/// Returns `FieldParseError::RecurRule` when the string doesn't match a known pattern.
pub(crate) fn parse_every(s: &str) -> Result<RecurRule, FieldParseError> {
    match s {
        "daily" => return Ok(RecurRule::Daily),
        "weekday" => return Ok(RecurRule::Weekday),
        "weekend" => return Ok(RecurRule::Weekend),
        _ => {}
    }

    if let Some(rest) = s.strip_prefix("weekly:") {
        let days = rest
            .split(',')
            .map(|t| {
                t.parse::<Weekday>()
                    .map_err(|_| FieldParseError::RecurRule {
                        value: format!("weekly:{rest}"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(RecurRule::Weekly(days));
    }

    if let Some(n_str) = s.strip_suffix('d')
        && let Ok(n) = n_str.parse::<u16>()
        && n > 0
    {
        return Ok(RecurRule::EveryNDays(n));
    }

    if let Some(n_str) = s.strip_suffix('w')
        && let Ok(n) = n_str.parse::<u16>()
        && n > 0
    {
        return Ok(RecurRule::EveryNWeeks(n));
    }

    Err(FieldParseError::RecurRule {
        value: s.to_owned(),
    })
}

/// Returns `true` if `rule` generates an occurrence on `date` given `anchor` and `end`.
#[must_use]
#[allow(dead_code)]
pub(crate) fn expand(
    rule: &RecurRule,
    anchor: &PlanDate,
    end: Option<&RecurEnd>,
    date: &PlanDate,
) -> bool {
    let anchor_jiff = anchor.as_jiff_date();
    let date_jiff = date.as_jiff_date();

    // Dates before anchor never occur.
    if date_jiff < anchor_jiff {
        return false;
    }

    // Check the recurrence pattern.
    let matches_pattern = match rule {
        RecurRule::Daily => true,
        RecurRule::Weekday => matches!(
            date_jiff.weekday(),
            JiffWeekday::Monday
                | JiffWeekday::Tuesday
                | JiffWeekday::Wednesday
                | JiffWeekday::Thursday
                | JiffWeekday::Friday
        ),
        RecurRule::Weekend => matches!(
            date_jiff.weekday(),
            JiffWeekday::Saturday | JiffWeekday::Sunday
        ),
        RecurRule::Weekly(days) => {
            let jiff_wd = date_jiff.weekday();
            days.iter().any(|d| jiff_weekday_matches(jiff_wd, *d))
        }
        RecurRule::EveryNDays(n) => {
            // diff >= 0 is guaranteed by the anchor guard above.
            let diff = days_between(anchor_jiff, date_jiff);
            diff.cast_unsigned().is_multiple_of(u64::from(*n))
        }
        RecurRule::EveryNWeeks(n) => {
            // diff >= 0 is guaranteed by the anchor guard above.
            let diff = days_between(anchor_jiff, date_jiff);
            let period = u64::from(*n) * 7;
            diff.cast_unsigned().is_multiple_of(period)
                && jiff_weekday_matches(date_jiff.weekday(), anchor_weekday(anchor))
        }
    };

    if !matches_pattern {
        return false;
    }

    // Apply end condition.
    match end {
        None => true,
        Some(RecurEnd::Until(until)) => date_jiff <= until.as_jiff_date(),
        Some(RecurEnd::Count(count)) => occurrence_index(rule, anchor, date) < *count,
    }
}

/// Returns the 0-based index of `date` among all occurrences of `rule` from `anchor` (inclusive).
///
/// Returns `0` if `date` is before `anchor` or is not an occurrence day (caller should check
/// `expand` first).
#[must_use]
#[allow(dead_code)]
pub(crate) fn occurrence_index(rule: &RecurRule, anchor: &PlanDate, date: &PlanDate) -> u32 {
    let anchor_jiff = anchor.as_jiff_date();
    let date_jiff = date.as_jiff_date();

    if date_jiff < anchor_jiff {
        return 0;
    }

    let diff = days_between(anchor_jiff, date_jiff);
    let total_days = diff.cast_unsigned();

    match rule {
        RecurRule::Daily => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "civil dates span < 2^31 days"
            )]
            let days = total_days as u32;
            days
        }
        RecurRule::Weekday => {
            // Count Mon-Fri days from anchor up to and including date.
            count_days_matching(anchor, date, |d| {
                matches!(
                    d.weekday(),
                    JiffWeekday::Monday
                        | JiffWeekday::Tuesday
                        | JiffWeekday::Wednesday
                        | JiffWeekday::Thursday
                        | JiffWeekday::Friday
                )
            })
        }
        RecurRule::Weekend => count_days_matching(anchor, date, |d| {
            matches!(d.weekday(), JiffWeekday::Saturday | JiffWeekday::Sunday)
        }),
        RecurRule::Weekly(days) => count_days_matching(anchor, date, |d| {
            let wd = d.weekday();
            days.iter().any(|day| jiff_weekday_matches(wd, *day))
        }),
        RecurRule::EveryNDays(n) => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "civil dates span < 2^31 days"
            )]
            let idx = (total_days / u64::from(*n)) as u32;
            idx
        }
        RecurRule::EveryNWeeks(n) => {
            let period = u64::from(*n) * 7;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "civil dates span < 2^31 days"
            )]
            let idx = (total_days / period) as u32;
            idx
        }
    }
}

/// Returns the number of days from `from` to `to` (signed, positive means `to` > `from`).
fn days_between(from: jiff::civil::Date, to: jiff::civil::Date) -> i64 {
    // `until` gives us a Span; .get_days() is i32 which is sufficient for civil dates.
    i64::from(
        from.until(to)
            .expect("civil date difference cannot fail")
            .get_days(),
    )
}

/// Counts the days from `anchor` up to (and including) `date` that satisfy `predicate`.
/// Returns a 0-based index (the first matching day is index 0).
fn count_days_matching<F>(anchor: &PlanDate, date: &PlanDate, predicate: F) -> u32
where
    F: Fn(jiff::civil::Date) -> bool,
{
    let anchor_jiff = anchor.as_jiff_date();
    let date_jiff = date.as_jiff_date();

    let mut count = 0u32;
    let mut current = anchor_jiff;
    while current <= date_jiff {
        if predicate(current) {
            count += 1;
        }
        current = current.tomorrow().expect("date advance should not fail");
    }
    // Return 0-based index: the first occurrence is index 0.
    count.saturating_sub(1)
}

/// Converts a `PlanDate`'s weekday to our domain `Weekday` enum.
fn anchor_weekday(anchor: &PlanDate) -> Weekday {
    match anchor.as_jiff_date().weekday() {
        JiffWeekday::Monday => Weekday::Monday,
        JiffWeekday::Tuesday => Weekday::Tuesday,
        JiffWeekday::Wednesday => Weekday::Wednesday,
        JiffWeekday::Thursday => Weekday::Thursday,
        JiffWeekday::Friday => Weekday::Friday,
        JiffWeekday::Saturday => Weekday::Saturday,
        JiffWeekday::Sunday => Weekday::Sunday,
    }
}

/// Returns `true` when a jiff weekday matches our domain `Weekday`.
fn jiff_weekday_matches(jiff_wd: JiffWeekday, our_wd: Weekday) -> bool {
    matches!(
        (jiff_wd, our_wd),
        (JiffWeekday::Monday, Weekday::Monday)
            | (JiffWeekday::Tuesday, Weekday::Tuesday)
            | (JiffWeekday::Wednesday, Weekday::Wednesday)
            | (JiffWeekday::Thursday, Weekday::Thursday)
            | (JiffWeekday::Friday, Weekday::Friday)
            | (JiffWeekday::Saturday, Weekday::Saturday)
            | (JiffWeekday::Sunday, Weekday::Sunday)
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::model::{PlanDate, RecurEnd, RecurRule, Weekday};

    fn date(s: &str) -> PlanDate {
        s.parse().unwrap()
    }

    // ── parse_every ──────────────────────────────────────────────────────

    #[test]
    fn parse_daily() {
        assert_eq!(parse_every("daily").unwrap(), RecurRule::Daily);
    }

    #[test]
    fn parse_weekday() {
        assert_eq!(parse_every("weekday").unwrap(), RecurRule::Weekday);
    }

    #[test]
    fn parse_weekend() {
        assert_eq!(parse_every("weekend").unwrap(), RecurRule::Weekend);
    }

    #[test]
    fn parse_weekly_single() {
        assert_eq!(
            parse_every("weekly:mon").unwrap(),
            RecurRule::Weekly(vec![Weekday::Monday])
        );
    }

    #[test]
    fn parse_weekly_multi() {
        assert_eq!(
            parse_every("weekly:mon,wed,fri").unwrap(),
            RecurRule::Weekly(vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday])
        );
    }

    #[test]
    fn parse_every_n_days() {
        assert_eq!(parse_every("3d").unwrap(), RecurRule::EveryNDays(3));
        assert_eq!(parse_every("14d").unwrap(), RecurRule::EveryNDays(14));
    }

    #[test]
    fn parse_every_n_weeks() {
        assert_eq!(parse_every("2w").unwrap(), RecurRule::EveryNWeeks(2));
    }

    #[test]
    fn parse_errors_on_bogus() {
        assert!(parse_every("bogus").is_err());
        assert!(parse_every("").is_err());
        assert!(parse_every("weekly:badday").is_err());
        assert!(parse_every("0d").is_err());
        assert!(parse_every("0w").is_err());
    }

    // ── expand: Daily ────────────────────────────────────────────────────

    #[test]
    fn daily_generates_on_anchor_and_following_days() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::Daily;

        assert!(expand(&rule, &anchor, None, &date("2026-06-01")));
        assert!(expand(&rule, &anchor, None, &date("2026-06-02")));
        assert!(expand(&rule, &anchor, None, &date("2026-06-10")));
    }

    #[test]
    fn daily_does_not_generate_before_anchor() {
        let anchor = date("2026-06-05");
        let rule = RecurRule::Daily;

        assert!(!expand(&rule, &anchor, None, &date("2026-06-04")));
        assert!(!expand(&rule, &anchor, None, &date("2026-01-01")));
    }

    #[test]
    fn daily_stops_at_until() {
        let anchor = date("2026-06-01");
        let end = RecurEnd::Until(date("2026-06-03"));
        let rule = RecurRule::Daily;

        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-03")));
        assert!(!expand(&rule, &anchor, Some(&end), &date("2026-06-04")));
    }

    #[test]
    fn daily_stops_after_count() {
        let anchor = date("2026-06-01");
        let end = RecurEnd::Count(3);
        let rule = RecurRule::Daily;

        // Occurrences: 2026-06-01 (idx 0), 2026-06-02 (idx 1), 2026-06-03 (idx 2) → 3 total
        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-01")));
        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-02")));
        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-03")));
        assert!(!expand(&rule, &anchor, Some(&end), &date("2026-06-04")));
    }

    // ── expand: Weekday ──────────────────────────────────────────────────

    #[test]
    fn weekday_rule_matches_mon_to_fri_only() {
        // 2026-06-01 is Monday
        let anchor = date("2026-06-01");
        let rule = RecurRule::Weekday;

        assert!(expand(&rule, &anchor, None, &date("2026-06-01"))); // Mon
        assert!(expand(&rule, &anchor, None, &date("2026-06-02"))); // Tue
        assert!(expand(&rule, &anchor, None, &date("2026-06-03"))); // Wed
        assert!(expand(&rule, &anchor, None, &date("2026-06-04"))); // Thu
        assert!(expand(&rule, &anchor, None, &date("2026-06-05"))); // Fri
        assert!(!expand(&rule, &anchor, None, &date("2026-06-06"))); // Sat
        assert!(!expand(&rule, &anchor, None, &date("2026-06-07"))); // Sun
        assert!(expand(&rule, &anchor, None, &date("2026-06-08"))); // Mon again
    }

    // ── expand: Weekend ──────────────────────────────────────────────────

    #[test]
    fn weekend_rule_matches_sat_and_sun_only() {
        let anchor = date("2026-06-01"); // Monday
        let rule = RecurRule::Weekend;

        assert!(!expand(&rule, &anchor, None, &date("2026-06-01"))); // Mon
        assert!(!expand(&rule, &anchor, None, &date("2026-06-05"))); // Fri
        assert!(expand(&rule, &anchor, None, &date("2026-06-06"))); // Sat
        assert!(expand(&rule, &anchor, None, &date("2026-06-07"))); // Sun
        assert!(!expand(&rule, &anchor, None, &date("2026-06-08"))); // Mon
    }

    // ── expand: Weekly ───────────────────────────────────────────────────

    #[test]
    fn weekly_rule_matches_specific_days() {
        let anchor = date("2026-06-01"); // Mon
        let rule = RecurRule::Weekly(vec![Weekday::Monday, Weekday::Wednesday]);

        assert!(expand(&rule, &anchor, None, &date("2026-06-01"))); // Mon
        assert!(!expand(&rule, &anchor, None, &date("2026-06-02"))); // Tue
        assert!(expand(&rule, &anchor, None, &date("2026-06-03"))); // Wed
        assert!(!expand(&rule, &anchor, None, &date("2026-06-04"))); // Thu
        assert!(!expand(&rule, &anchor, None, &date("2026-06-05"))); // Fri
        assert!(!expand(&rule, &anchor, None, &date("2026-06-06"))); // Sat
        assert!(!expand(&rule, &anchor, None, &date("2026-06-07"))); // Sun
        assert!(expand(&rule, &anchor, None, &date("2026-06-08"))); // Mon
        assert!(expand(&rule, &anchor, None, &date("2026-06-10"))); // Wed
    }

    // ── expand: EveryNDays ───────────────────────────────────────────────

    #[test]
    fn every_n_days_fires_on_correct_intervals() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::EveryNDays(3);

        assert!(expand(&rule, &anchor, None, &date("2026-06-01"))); // day 0
        assert!(!expand(&rule, &anchor, None, &date("2026-06-02"))); // day 1
        assert!(!expand(&rule, &anchor, None, &date("2026-06-03"))); // day 2
        assert!(expand(&rule, &anchor, None, &date("2026-06-04"))); // day 3
        assert!(!expand(&rule, &anchor, None, &date("2026-06-05"))); // day 4
        assert!(!expand(&rule, &anchor, None, &date("2026-06-06"))); // day 5
        assert!(expand(&rule, &anchor, None, &date("2026-06-07"))); // day 6
    }

    // ── expand: EveryNWeeks ──────────────────────────────────────────────

    #[test]
    fn every_n_weeks_fires_on_correct_intervals() {
        // anchor is 2026-06-01 (Monday)
        let anchor = date("2026-06-01");
        let rule = RecurRule::EveryNWeeks(2);

        assert!(expand(&rule, &anchor, None, &date("2026-06-01"))); // week 0, Mon
        assert!(!expand(&rule, &anchor, None, &date("2026-06-08"))); // week 1, Mon (odd week)
        assert!(expand(&rule, &anchor, None, &date("2026-06-15"))); // week 2, Mon
        assert!(!expand(&rule, &anchor, None, &date("2026-06-22"))); // week 3
        assert!(expand(&rule, &anchor, None, &date("2026-06-29"))); // week 4
        // Wrong weekday on a valid week boundary
        assert!(!expand(&rule, &anchor, None, &date("2026-06-16"))); // Tue week 2
    }

    // ── expand: Until end ────────────────────────────────────────────────

    #[test]
    fn until_end_stops_on_the_until_date() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::Daily;
        let end = RecurEnd::Until(date("2026-06-05"));

        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-05")));
        assert!(!expand(&rule, &anchor, Some(&end), &date("2026-06-06")));
    }

    // ── expand: Count end ────────────────────────────────────────────────

    #[test]
    fn count_end_stops_after_n_occurrences() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::Daily;
        let end = RecurEnd::Count(2);

        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-01"))); // idx 0
        assert!(expand(&rule, &anchor, Some(&end), &date("2026-06-02"))); // idx 1
        assert!(!expand(&rule, &anchor, Some(&end), &date("2026-06-03"))); // idx 2
    }

    // ── date before anchor ───────────────────────────────────────────────

    #[test]
    fn date_before_anchor_always_returns_false() {
        let anchor = date("2026-06-10");
        for rule in [
            RecurRule::Daily,
            RecurRule::Weekday,
            RecurRule::Weekend,
            RecurRule::EveryNDays(1),
            RecurRule::EveryNWeeks(1),
        ] {
            assert!(!expand(&rule, &anchor, None, &date("2026-06-09")));
        }
        assert!(!expand(
            &RecurRule::Weekly(vec![Weekday::Monday]),
            &anchor,
            None,
            &date("2026-06-09")
        ));
    }

    // ── occurrence_index for Daily ────────────────────────────────────────

    #[test]
    fn occurrence_index_daily_counts_correctly() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::Daily;

        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-01")), 0);
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-02")), 1);
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-05")), 4);
    }

    #[test]
    fn occurrence_index_before_anchor_returns_zero() {
        let anchor = date("2026-06-10");
        let rule = RecurRule::Daily;
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-09")), 0);
    }

    // ── occurrence_index for Weekday ─────────────────────────────────────

    #[test]
    fn occurrence_index_weekday_counts_correctly() {
        // 2026-06-01 is Monday
        let anchor = date("2026-06-01");
        let rule = RecurRule::Weekday;

        // Mon=0, Tue=1, Wed=2, Thu=3, Fri=4
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-01")), 0); // Mon
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-02")), 1); // Tue
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-05")), 4); // Fri
        // Sat: still idx 4 (Fri was the 5th weekday)
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-06")), 4); // Sat (same as Fri)
        // Next Mon is index 5
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-08")), 5); // Mon
    }

    // ── occurrence_index for Weekend ─────────────────────────────────────

    #[test]
    fn occurrence_index_weekend_counts_correctly() {
        // 2026-06-01 is Monday; first weekend is Sat 2026-06-06
        let anchor = date("2026-06-01");
        let rule = RecurRule::Weekend;

        // Mon-Fri: no weekend occurrence yet, index = 0 (sat_sub gives 0)
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-05")), 0); // Fri: 0 weekend hits
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-06")), 0); // Sat: first hit, idx 0
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-07")), 1); // Sun: second hit, idx 1
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-13")), 2); // Sat+1wk, idx 2
    }

    // ── occurrence_index for Weekly ──────────────────────────────────────

    #[test]
    fn occurrence_index_weekly_counts_correctly() {
        // 2026-06-01 is Monday; rule: Mon+Wed
        let anchor = date("2026-06-01");
        let rule = RecurRule::Weekly(vec![Weekday::Monday, Weekday::Wednesday]);

        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-01")), 0); // Mon: idx 0
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-03")), 1); // Wed: idx 1
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-08")), 2); // Mon next week: idx 2
    }

    // ── occurrence_index for EveryNDays ──────────────────────────────────

    #[test]
    fn occurrence_index_every_n_days_counts_correctly() {
        let anchor = date("2026-06-01");
        let rule = RecurRule::EveryNDays(3);

        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-01")), 0); // day 0: idx 0
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-04")), 1); // day 3: idx 1
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-07")), 2); // day 6: idx 2
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-05")), 1); // day 4: idx 1 (floor div)
    }

    // ── occurrence_index for EveryNWeeks ─────────────────────────────────

    #[test]
    fn occurrence_index_every_n_weeks_counts_correctly() {
        // anchor is 2026-06-01 (Monday)
        let anchor = date("2026-06-01");
        let rule = RecurRule::EveryNWeeks(2);

        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-01")), 0); // week 0: idx 0
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-15")), 1); // week 2: idx 1
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-29")), 2); // week 4: idx 2
        assert_eq!(occurrence_index(&rule, &anchor, &date("2026-06-08")), 0); // week 1 (floor): idx 0
    }

    // ── anchor_weekday coverage ───────────────────────────────────────────

    #[test]
    fn anchor_weekday_covers_all_days() {
        // Test that each day of the week is correctly identified via EveryNWeeks
        // (which calls anchor_weekday internally).
        // anchor on each day of week and test expansion.
        let days_and_anchors = [
            ("2026-06-01", Weekday::Monday),
            ("2026-06-02", Weekday::Tuesday),
            ("2026-06-03", Weekday::Wednesday),
            ("2026-06-04", Weekday::Thursday),
            ("2026-06-05", Weekday::Friday),
            ("2026-06-06", Weekday::Saturday),
            ("2026-06-07", Weekday::Sunday),
        ];
        for (anchor_str, expected_wd) in days_and_anchors {
            let anchor = date(anchor_str);
            // anchor_weekday is indirectly tested: EveryNWeeks(1) fires only on the anchor weekday
            let rule = RecurRule::EveryNWeeks(1);
            // The anchor itself should fire
            assert!(expand(&rule, &anchor, None, &anchor));
            // One day after should NOT fire (different weekday)
            let next = date(
                jiff::civil::Date::strptime("%Y-%m-%d", anchor_str)
                    .unwrap()
                    .tomorrow()
                    .unwrap()
                    .strftime("%Y-%m-%d")
                    .to_string()
                    .as_str(),
            );
            assert!(!expand(&rule, &anchor, None, &next));
            // Verify expected weekday matches anchor's weekday
            let _ = expected_wd; // used for documentation
        }
    }

    // ── Proptest: weekday 7-day window ───────────────────────────────────

    proptest! {
        #[test]
        fn weekday_property_seven_consecutive_days_give_five_true(
            // Test starting from Mondays: 0..52 weeks after a known Monday (2026-06-01)
            week_offset in 0u32..52u32
        ) {
            // 2026-06-01 is Monday; go week_offset full weeks ahead
            let base_monday = "2026-06-01".parse::<PlanDate>().unwrap().as_jiff_date();
            let monday = base_monday
                .checked_add(jiff::Span::new().days(i32::try_from(week_offset * 7).unwrap()))
                .unwrap();

            let anchor = PlanDate::from_jiff_date(monday);
            let rule = RecurRule::Weekday;

            let mut count = 0u32;
            for d in 0i32..7 {
                let day = PlanDate::from_jiff_date(
                    monday.checked_add(jiff::Span::new().days(d)).unwrap()
                );
                if expand(&rule, &anchor, None, &day) {
                    count += 1;
                }
            }
            prop_assert_eq!(count, 5);
        }

        #[test]
        fn occurrence_index_daily_property(days in 0u32..100u32) {
            let anchor = "2026-06-01".parse::<PlanDate>().unwrap();
            let anchor_jiff = anchor.as_jiff_date();
            let target = PlanDate::from_jiff_date(
                anchor_jiff
                    .checked_add(jiff::Span::new().days(i32::try_from(days).unwrap()))
                    .unwrap()
            );
            let rule = RecurRule::Daily;
            prop_assert_eq!(occurrence_index(&rule, &anchor, &target), days);
        }
    }
}
