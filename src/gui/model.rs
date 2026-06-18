//! Pure view-model builders for the Cockpit GUI. No IO. Fully unit-tested.

use jiff::Timestamp;

use crate::{
    model::{Approval, Plan, Status},
    store::FireRecord,
    time::{resolve_block_end, resolve_block_start},
};

fn format_clock(seconds: u32) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    format!("{h:02}:{m:02}")
}

fn countdown_str(starts_in_secs: i64) -> String {
    if starts_in_secs <= 0 {
        return "now".to_owned();
    }
    let hours = starts_in_secs / 3_600;
    let minutes = (starts_in_secs % 3_600) / 60;
    if hours > 0 {
        format!("in {hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("in {minutes}m")
    } else {
        format!("in {starts_in_secs}s")
    }
}

/// A single block rendered in the Today timeline.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct BlockCardModel {
    pub id: String,
    pub title: String,
    pub time_range: String,
    pub countdown: String,
    pub status: Status,
    pub has_recurrence: bool,
    pub has_run: bool,
    pub awaiting_approval: bool,
    pub has_agent: bool,
    pub has_expect_by_breach: bool,
    pub tags: Vec<String>,
}

/// Data for the Today timeline panel.
#[derive(Debug, Clone, PartialEq)]
pub struct TodayModel {
    pub date_label: String,
    pub now_label: String,
    pub cards: Vec<BlockCardModel>,
}

/// Builds the Today timeline model from a plan and the current instant.
#[must_use]
pub fn build_today_model(plan: &Plan, now: Timestamp) -> TodayModel {
    let date_label = plan.date.to_string();
    let tz = plan
        .timezone
        .to_time_zone()
        .unwrap_or(jiff::tz::TimeZone::UTC);
    let zoned = now.to_zoned(tz);
    let now_label = format!("{:02}:{:02}", zoned.hour(), zoned.minute());

    let cards = plan
        .blocks
        .iter()
        .map(|block| {
            let start = resolve_block_start(plan, block).unwrap_or(now);
            let end = resolve_block_end(plan, block).unwrap_or(now);

            let time_range = format!(
                "{}–{}",
                format_clock(block.start.seconds_since_midnight()),
                format_clock(block.span.resolved_end_seconds(block.start))
            );

            let countdown = if block.status.is_terminal() {
                block.status.as_str().to_owned()
            } else if start <= now && now < end {
                "now".to_owned()
            } else if start > now {
                countdown_str(start.duration_since(now).as_secs())
            } else {
                "ended".to_owned()
            };

            let awaiting_approval =
                matches!(block.approval, Some(Approval::Pending)) && block.run.is_some();

            let has_expect_by_breach = block.expect_by.as_ref().is_some_and(|expect_by| {
                let elapsed = now.duration_since(start).as_secs();
                elapsed > i64::from(expect_by.as_seconds()) && !block.status.is_terminal()
            });

            BlockCardModel {
                id: block.id.to_string(),
                title: block.title.clone(),
                time_range,
                countdown,
                status: block.status,
                has_recurrence: block.recurrence.is_some(),
                has_run: block.run.is_some(),
                awaiting_approval,
                has_agent: block.agent.is_some(),
                has_expect_by_breach,
                tags: block.tags.clone(),
            }
        })
        .collect();

    TodayModel {
        date_label,
        now_label,
        cards,
    }
}

/// A single pending-approval item in the Approvals panel.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalItemModel {
    pub id: String,
    pub title: String,
    pub when: String,
    pub argv: String,
}

/// Data for the Approvals panel (blocks with `run:` awaiting approval).
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalsModel {
    pub items: Vec<ApprovalItemModel>,
}

/// Builds the Approvals model from a plan.
#[must_use]
pub fn build_approvals_model(plan: &Plan) -> ApprovalsModel {
    let items = plan
        .blocks
        .iter()
        .filter(|b| matches!(b.approval, Some(Approval::Pending)) && b.run.is_some())
        .map(|b| ApprovalItemModel {
            id: b.id.to_string(),
            title: b.title.clone(),
            when: format_clock(b.start.seconds_since_midnight()),
            argv: b
                .run
                .as_ref()
                .map_or_else(String::new, |r| r.as_slice().join(" ")),
        })
        .collect();
    ApprovalsModel { items }
}

/// Visual category for an activity feed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Ok,
    Run,
    Error,
    Info,
}

/// A single entry in the Activity (fire-ledger) feed.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityItemModel {
    pub icon: &'static str,
    pub ts_label: String,
    pub text: String,
    pub kind: ActivityKind,
}

/// Data for the Activity feed panel.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityModel {
    pub items: Vec<ActivityItemModel>,
}

fn outcome_style(outcome: &str) -> (&'static str, ActivityKind) {
    match outcome {
        "notify" | "close" => ("✓", ActivityKind::Ok),
        "activate" => ("▶", ActivityKind::Run),
        "missed" => ("✗", ActivityKind::Error),
        _ => ("·", ActivityKind::Info),
    }
}

/// Builds the Activity feed model from fire-ledger records (shown newest-first).
#[must_use]
pub fn build_activity_model(records: &[FireRecord]) -> ActivityModel {
    let items = records
        .iter()
        .rev()
        .map(|r| {
            let (icon, kind) = outcome_style(&r.outcome);
            let ts_label = r.ts.to_string();
            let text = if r.detail.is_empty() {
                format!("{} {}", r.id, r.outcome)
            } else {
                format!("{} {} — {}", r.id, r.outcome, r.detail)
            };
            ActivityItemModel {
                icon,
                ts_label,
                text,
                kind,
            }
        })
        .collect();
    ActivityModel { items }
}

/// Navigation tabs in the left rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Today,
    Upcoming,
    Automations,
    Agents,
    Activity,
    Approvals,
}

/// Data for the left-rail nav.
#[derive(Debug, Clone, PartialEq)]
pub struct NavModel {
    pub active_tab: NavTab,
    pub pending_approvals_count: usize,
}

/// Builds the nav model from the active tab and number of pending approvals.
#[must_use]
pub fn build_nav_model(active_tab: NavTab, pending_approvals_count: usize) -> NavModel {
    NavModel {
        active_tab,
        pending_approvals_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lifecycle::Event, model::Plan, store::FireRecord};

    const PLAN_BASE: &str = r#"
date = "2026-06-08"
timezone = "UTC"
"#;

    fn plan_with_block(extra: &str) -> Plan {
        Plan::from_toml(&format!("{PLAN_BASE}{extra}")).unwrap()
    }

    fn make_record(outcome: &str, detail: &str) -> FireRecord {
        FireRecord {
            ts: "2026-06-08T09:00:00Z".parse().unwrap(),
            date: "2026-06-08".parse().unwrap(),
            id: "focus-1".parse().unwrap(),
            event: Event::Notify,
            outcome: outcome.to_owned(),
            detail: detail.to_owned(),
            agent: None,
        }
    }

    fn now_at(hhmm: &str) -> Timestamp {
        format!("2026-06-08T{hhmm}:00Z").parse().unwrap()
    }

    // ── countdown_str ──────────────────────────────────────────────────────────

    #[test]
    fn countdown_str_zero_or_negative_is_now() {
        assert_eq!(countdown_str(0), "now");
        assert_eq!(countdown_str(-10), "now");
    }

    #[test]
    fn countdown_str_seconds_only() {
        assert_eq!(countdown_str(45), "in 45s");
    }

    #[test]
    fn countdown_str_minutes() {
        assert_eq!(countdown_str(300), "in 5m");
    }

    #[test]
    fn countdown_str_hours_and_minutes() {
        assert_eq!(countdown_str(7200), "in 2h00m");
        assert_eq!(countdown_str(7500), "in 2h05m");
    }

    // ── build_today_model ──────────────────────────────────────────────────────

    #[test]
    fn today_model_empty_plan() {
        let plan = Plan::from_toml(PLAN_BASE).unwrap();
        let model = build_today_model(&plan, now_at("10:00"));
        assert_eq!(model.date_label, "2026-06-08");
        assert_eq!(model.now_label, "10:00");
        assert!(model.cards.is_empty());
    }

    #[test]
    fn today_model_pending_future_countdown_seconds() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "10:00"
duration = "45m"
"#,
        );
        // 50 seconds before start
        let model = build_today_model(&plan, now_at("09:59"));
        assert_eq!(model.cards[0].countdown, "in 1m");
        assert_eq!(model.cards[0].status, Status::Pending);
        assert!(!model.cards[0].has_recurrence);
        assert!(!model.cards[0].has_run);
    }

    #[test]
    fn today_model_pending_future_countdown_hours() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "14:00"
duration = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("10:00"));
        assert_eq!(model.cards[0].countdown, "in 4h00m");
    }

    #[test]
    fn today_model_active_block_shows_now() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "45m"
"#,
        );
        let model = build_today_model(&plan, now_at("09:15"));
        assert_eq!(model.cards[0].countdown, "now");
    }

    #[test]
    fn today_model_ended_block_shows_ended() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("10:00"));
        assert_eq!(model.cards[0].countdown, "ended");
    }

    #[test]
    fn today_model_terminal_statuses() {
        for (status, label) in [
            ("done", "done"),
            ("skipped", "skipped"),
            ("missed", "missed"),
            ("expired", "expired"),
        ] {
            let plan = plan_with_block(&format!(
                r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
status = "{status}"
"#
            ));
            let model = build_today_model(&plan, now_at("10:00"));
            assert_eq!(model.cards[0].countdown, label, "status={status}");
        }
    }

    #[test]
    fn today_model_active_status_label() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
status = "active"
"#,
        );
        let model = build_today_model(&plan, now_at("09:15"));
        assert_eq!(model.cards[0].status, Status::Active);
    }

    #[test]
    fn today_model_recurrence_badge() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "standup-1"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert!(model.cards[0].has_recurrence);
    }

    #[test]
    fn today_model_run_and_approval_badges() {
        // run: without explicit approval defaults to pending
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Automation"
start = "09:00"
duration = "30m"
run = ["/usr/bin/true"]
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert!(model.cards[0].has_run);
        assert!(model.cards[0].awaiting_approval);
    }

    #[test]
    fn today_model_approved_run_is_not_awaiting() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Automation"
start = "09:00"
duration = "30m"
run = ["/usr/bin/true"]
approval = "approved"
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert!(model.cards[0].has_run);
        assert!(!model.cards[0].awaiting_approval);
    }

    #[test]
    fn today_model_agent_badge() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "agent-1"
title = "Agent task"
start = "09:00"
duration = "30m"
agent = "my-agent"
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert!(model.cards[0].has_agent);
    }

    #[test]
    fn today_model_expect_by_breach() {
        // Block at 09:00, expect_by = 30m; now = 10:00 → 60m elapsed > 30m → breach
        let plan = plan_with_block(
            r#"
[[block]]
id = "critical-1"
title = "Critical"
start = "09:00"
duration = "15m"
expect_by = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("10:00"));
        assert!(model.cards[0].has_expect_by_breach);
    }

    #[test]
    fn today_model_expect_by_no_breach() {
        // Block at 09:00, expect_by = 2h; now = 09:30 → 30m elapsed < 2h → no breach
        let plan = plan_with_block(
            r#"
[[block]]
id = "critical-1"
title = "Critical"
start = "09:00"
duration = "15m"
expect_by = "2h"
"#,
        );
        let model = build_today_model(&plan, now_at("09:30"));
        assert!(!model.cards[0].has_expect_by_breach);
    }

    #[test]
    fn today_model_expect_by_absent() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("09:30"));
        assert!(!model.cards[0].has_expect_by_breach);
    }

    #[test]
    fn today_model_time_range_format() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
end = "09:45"
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert_eq!(model.cards[0].time_range, "09:00–09:45");
    }

    #[test]
    fn today_model_tags() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
tags = ["deep-work", "morning"]
"#,
        );
        let model = build_today_model(&plan, now_at("08:00"));
        assert_eq!(model.cards[0].tags, vec!["deep-work", "morning"]);
    }

    // ── build_approvals_model ──────────────────────────────────────────────────

    #[test]
    fn approvals_model_empty_when_no_pending() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
"#,
        );
        let model = build_approvals_model(&plan);
        assert!(model.items.is_empty());
    }

    #[test]
    fn approvals_model_lists_pending_run_blocks() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Automation"
start = "09:00"
duration = "30m"
run = ["/usr/bin/sync", "--fast"]
"#,
        );
        let model = build_approvals_model(&plan);
        assert_eq!(model.items.len(), 1);
        assert_eq!(model.items[0].id, "auto-1");
        assert_eq!(model.items[0].when, "09:00");
        assert_eq!(model.items[0].argv, "/usr/bin/sync --fast");
    }

    // ── build_activity_model ───────────────────────────────────────────────────

    #[test]
    fn activity_model_empty() {
        let model = build_activity_model(&[]);
        assert!(model.items.is_empty());
    }

    #[test]
    fn activity_model_notify_outcome() {
        let model = build_activity_model(&[make_record("notify", "")]);
        assert_eq!(model.items[0].icon, "✓");
        assert_eq!(model.items[0].kind, ActivityKind::Ok);
        assert_eq!(model.items[0].text, "focus-1 notify");
    }

    #[test]
    fn activity_model_close_outcome() {
        let model = build_activity_model(&[make_record("close", "done")]);
        assert_eq!(model.items[0].icon, "✓");
        assert_eq!(model.items[0].kind, ActivityKind::Ok);
        assert_eq!(model.items[0].text, "focus-1 close — done");
    }

    #[test]
    fn activity_model_activate_outcome() {
        let model = build_activity_model(&[make_record("activate", "")]);
        assert_eq!(model.items[0].icon, "▶");
        assert_eq!(model.items[0].kind, ActivityKind::Run);
    }

    #[test]
    fn activity_model_missed_outcome() {
        let model = build_activity_model(&[make_record("missed", "")]);
        assert_eq!(model.items[0].icon, "✗");
        assert_eq!(model.items[0].kind, ActivityKind::Error);
    }

    #[test]
    fn activity_model_other_outcome() {
        let model = build_activity_model(&[make_record("no-op", "no-op")]);
        assert_eq!(model.items[0].icon, "·");
        assert_eq!(model.items[0].kind, ActivityKind::Info);
    }

    #[test]
    fn activity_model_newest_first() {
        let r1 = make_record("notify", "");
        let r2 = make_record("missed", "");
        let model = build_activity_model(&[r1, r2]);
        // rev() → r2 first
        assert_eq!(model.items[0].kind, ActivityKind::Error);
        assert_eq!(model.items[1].kind, ActivityKind::Ok);
    }

    // ── outcome_style ──────────────────────────────────────────────────────────

    #[test]
    fn outcome_style_covers_all_arms() {
        assert_eq!(outcome_style("notify").1, ActivityKind::Ok);
        assert_eq!(outcome_style("close").1, ActivityKind::Ok);
        assert_eq!(outcome_style("activate").1, ActivityKind::Run);
        assert_eq!(outcome_style("missed").1, ActivityKind::Error);
        assert_eq!(outcome_style("no-op").1, ActivityKind::Info);
    }

    // ── build_nav_model ────────────────────────────────────────────────────────

    #[test]
    fn nav_model_all_tabs() {
        for tab in [
            NavTab::Today,
            NavTab::Upcoming,
            NavTab::Automations,
            NavTab::Agents,
            NavTab::Activity,
            NavTab::Approvals,
        ] {
            let model = build_nav_model(tab, 0);
            assert_eq!(model.active_tab, tab);
            assert_eq!(model.pending_approvals_count, 0);
        }
    }

    #[test]
    fn nav_model_pending_approvals_count() {
        let model = build_nav_model(NavTab::Today, 3);
        assert_eq!(model.pending_approvals_count, 3);
    }
}
