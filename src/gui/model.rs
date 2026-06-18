//! Pure view-model builders for the Cockpit GUI. No IO. Fully unit-tested.

use std::collections::HashMap;

use jiff::Timestamp;

use crate::{
    model::{Approval, Block, Plan, RecurEnd, RecurRule, RecurringRules, Status, Weekday},
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

fn format_weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "mon",
        Weekday::Tuesday => "tue",
        Weekday::Wednesday => "wed",
        Weekday::Thursday => "thu",
        Weekday::Friday => "fri",
        Weekday::Saturday => "sat",
        Weekday::Sunday => "sun",
    }
}

fn format_recur_rule(rule: &RecurRule) -> String {
    match rule {
        RecurRule::Daily => "daily".to_owned(),
        RecurRule::Weekday => "weekday".to_owned(),
        RecurRule::Weekend => "weekend".to_owned(),
        RecurRule::Weekly(days) => {
            let day_strs: Vec<_> = days.iter().copied().map(format_weekday).collect();
            format!("weekly:{}", day_strs.join(","))
        }
        RecurRule::EveryNDays(n) => format!("every {n} days"),
        RecurRule::EveryNWeeks(n) => format!("every {n} weeks"),
    }
}

/// A single block rendered in the Today/Upcoming timeline.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
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

/// Builds a single block card model — shared by Today, Upcoming, and Up-Next builders.
#[must_use]
pub fn build_block_card_model(block: &Block, plan: &Plan, now: Timestamp) -> BlockCardModel {
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
}

/// Data for the Today timeline panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TodayModel {
    pub date_label: String,
    pub now_label: String,
    /// Index into `cards` before which the "now" line should be drawn (None = after all cards).
    pub now_line_index: Option<usize>,
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

    let cards: Vec<BlockCardModel> = plan
        .blocks
        .iter()
        .map(|block| build_block_card_model(block, plan, now))
        .collect();

    // Find where the "now" line sits: before the first block that starts after `now`.
    let now_line_index = cards.iter().enumerate().find_map(|(i, card)| {
        if card.countdown.starts_with("in ") {
            Some(i)
        } else {
            None
        }
    });

    TodayModel {
        date_label,
        now_label,
        now_line_index,
        cards,
    }
}

/// A single pending-approval item in the Approvals panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ApprovalItemModel {
    pub id: String,
    pub title: String,
    pub when: String,
    pub argv: String,
    /// Derived from the block's agent field (e.g. "Scheduled by: agent-name").
    pub reason: Option<String>,
}

/// Data for the Approvals panel (blocks with `run:` awaiting approval).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
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
            reason: b.agent.as_ref().map(|a| format!("Scheduled by: {a}")),
        })
        .collect();
    ApprovalsModel { items }
}

/// Visual category for an activity feed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ActivityKind {
    Ok,
    Run,
    Error,
    Info,
}

/// A single entry in the Activity (fire-ledger) feed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ActivityItemModel {
    pub icon: &'static str,
    pub ts_label: String,
    pub text: String,
    pub kind: ActivityKind,
}

/// Data for the Activity feed panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum NavTab {
    Today,
    Upcoming,
    Automations,
    Agents,
    Activity,
    Approvals,
}

/// Data for the left-rail nav.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
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

// ── Upcoming ─────────────────────────────────────────────────────────────────

/// A single future day shown in the Upcoming panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UpcomingDay {
    pub date_label: String,
    pub cards: Vec<BlockCardModel>,
}

/// Data for the Upcoming panel (next N days' plans).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UpcomingModel {
    pub days: Vec<UpcomingDay>,
}

/// Builds the Upcoming model from a slice of future plans.
#[must_use]
pub fn build_upcoming_model(plans: &[Plan], now: Timestamp) -> UpcomingModel {
    let days = plans
        .iter()
        .map(|plan| {
            let date_label = plan.date.to_string();
            let cards = plan
                .blocks
                .iter()
                .map(|b| build_block_card_model(b, plan, now))
                .collect();
            UpcomingDay { date_label, cards }
        })
        .collect();
    UpcomingModel { days }
}

// ── Automations ───────────────────────────────────────────────────────────────

/// A single recurring rule shown in the Automations panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AutomationRuleModel {
    pub id: String,
    pub title: String,
    pub schedule: String,
    pub end_label: Option<String>,
    pub has_run: bool,
    pub has_agent: bool,
}

/// Data for the Automations panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AutomationsModel {
    pub rules: Vec<AutomationRuleModel>,
}

/// Builds the Automations panel model from the recurring rules store.
#[must_use]
pub fn build_automations_model(rules: &RecurringRules) -> AutomationsModel {
    let rule_models = rules
        .blocks
        .iter()
        .map(|block| {
            let (schedule, end_label) = if let Some(rec) = &block.recurrence {
                let sched = format_recur_rule(&rec.rule);
                let end = rec.end.as_ref().map(|e| match e {
                    RecurEnd::Until(date) => format!("until {date}"),
                    RecurEnd::Count(n) => format!("{n} occurrences"),
                });
                (sched, end)
            } else {
                ("(no schedule)".to_owned(), None)
            };
            AutomationRuleModel {
                id: block.id.to_string(),
                title: block.title.clone(),
                schedule,
                end_label,
                has_run: block.run.is_some(),
                has_agent: block.agent.is_some(),
            }
        })
        .collect();
    AutomationsModel { rules: rule_models }
}

// ── Agents ────────────────────────────────────────────────────────────────────

/// A single agent's summary in the Agents panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AgentStatusModel {
    pub name: String,
    pub last_action: String,
    pub is_ok: bool,
}

/// Data for the Agents panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AgentsModel {
    pub agents: Vec<AgentStatusModel>,
}

/// Builds the Agents panel model from the fire-ledger records.
#[must_use]
pub fn build_agents_model(records: &[FireRecord]) -> AgentsModel {
    // Most-recent record per agent (records are append-only, so last in slice wins).
    let mut by_agent: HashMap<String, &FireRecord> = HashMap::new();
    for record in records {
        if let Some(ref agent) = record.agent {
            by_agent.insert(agent.clone(), record);
        }
    }
    let mut agents: Vec<AgentStatusModel> = by_agent
        .into_iter()
        .map(|(name, rec)| {
            let ts = rec.ts.to_string();
            let ts_short = ts.get(..16).unwrap_or(&ts);
            let last_action = format!("{ts_short} {}", rec.outcome);
            let is_ok = rec.outcome != "missed" && !rec.outcome.contains("fail");
            AgentStatusModel {
                name,
                last_action,
                is_ok,
            }
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    AgentsModel { agents }
}

// ── Block detail (right context pane) ────────────────────────────────────────

/// Detailed information about a selected block for the right context pane.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BlockDetailModel {
    pub id: String,
    pub title: String,
    pub time_range: String,
    pub countdown: String,
    pub status: Status,
    pub recurrence_label: Option<String>,
    pub run_argv: Option<String>,
    pub after_ids: Vec<String>,
    pub agent: Option<String>,
    pub approval: Option<String>,
    pub tags: Vec<String>,
}

/// Builds the block detail model for the right context pane.
#[must_use]
pub fn build_block_detail_model(block: &Block, plan: &Plan, now: Timestamp) -> BlockDetailModel {
    let card = build_block_card_model(block, plan, now);

    let recurrence_label = block.recurrence.as_ref().map(|r| {
        let base = format_recur_rule(&r.rule);
        match &r.end {
            None => base,
            Some(RecurEnd::Until(date)) => format!("{base}, until {date}"),
            Some(RecurEnd::Count(n)) => format!("{base}, {n}×"),
        }
    });

    let run_argv = block.run.as_ref().map(|r| r.as_slice().join(" "));

    let approval = block.approval.map(|a| match a {
        Approval::Pending => "pending".to_owned(),
        Approval::Approved => "approved".to_owned(),
    });

    BlockDetailModel {
        id: block.id.to_string(),
        title: block.title.clone(),
        time_range: card.time_range,
        countdown: card.countdown,
        status: block.status,
        recurrence_label,
        run_argv,
        after_ids: block.after.iter().map(ToString::to_string).collect(),
        agent: block.agent.clone(),
        approval,
        tags: block.tags.clone(),
    }
}

// ── Up-next (default right pane when nothing selected) ───────────────────────

/// Next 3 upcoming blocks shown in the right pane when nothing is selected.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UpNextModel {
    pub items: Vec<BlockCardModel>,
}

/// Builds the "Up next" model — the first (up to 3) non-terminal blocks that start after `now`.
#[must_use]
pub fn build_up_next_model(plan: &Plan, now: Timestamp) -> UpNextModel {
    let items: Vec<_> = plan
        .blocks
        .iter()
        .filter(|b| {
            let start = resolve_block_start(plan, b).unwrap_or(now);
            !b.status.is_terminal() && start >= now
        })
        .take(3)
        .map(|b| build_block_card_model(b, plan, now))
        .collect();
    UpNextModel { items }
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

    fn make_agent_record(agent: &str, outcome: &str) -> FireRecord {
        FireRecord {
            ts: "2026-06-08T09:00:00Z".parse().unwrap(),
            date: "2026-06-08".parse().unwrap(),
            id: "task-1".parse().unwrap(),
            event: Event::Notify,
            outcome: outcome.to_owned(),
            detail: String::new(),
            agent: Some(agent.to_owned()),
        }
    }

    fn now_at(hhmm: &str) -> Timestamp {
        format!("2026-06-08T{hhmm}:00Z").parse().unwrap()
    }

    // ── format helpers ────────────────────────────────────────────────────────

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

    #[test]
    fn format_weekday_covers_all_variants() {
        assert_eq!(format_weekday(Weekday::Monday), "mon");
        assert_eq!(format_weekday(Weekday::Tuesday), "tue");
        assert_eq!(format_weekday(Weekday::Wednesday), "wed");
        assert_eq!(format_weekday(Weekday::Thursday), "thu");
        assert_eq!(format_weekday(Weekday::Friday), "fri");
        assert_eq!(format_weekday(Weekday::Saturday), "sat");
        assert_eq!(format_weekday(Weekday::Sunday), "sun");
    }

    #[test]
    fn format_recur_rule_covers_all_variants() {
        use crate::model::RecurRule;
        assert_eq!(format_recur_rule(&RecurRule::Daily), "daily");
        assert_eq!(format_recur_rule(&RecurRule::Weekday), "weekday");
        assert_eq!(format_recur_rule(&RecurRule::Weekend), "weekend");
        assert_eq!(
            format_recur_rule(&RecurRule::Weekly(vec![Weekday::Monday, Weekday::Friday])),
            "weekly:mon,fri"
        );
        assert_eq!(format_recur_rule(&RecurRule::EveryNDays(3)), "every 3 days");
        assert_eq!(
            format_recur_rule(&RecurRule::EveryNWeeks(2)),
            "every 2 weeks"
        );
    }

    // ── build_block_card_model ────────────────────────────────────────────────

    #[test]
    fn block_card_pending_future_countdown() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "10:00"
duration = "45m"
"#,
        );
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("09:59"));
        assert_eq!(card.countdown, "in 1m");
        assert_eq!(card.status, Status::Pending);
        assert!(!card.has_recurrence);
        assert!(!card.has_run);
    }

    #[test]
    fn block_card_active_shows_now() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "45m"
"#,
        );
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("09:15"));
        assert_eq!(card.countdown, "now");
    }

    #[test]
    fn block_card_ended_shows_ended() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
"#,
        );
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("10:00"));
        assert_eq!(card.countdown, "ended");
    }

    #[test]
    fn block_card_terminal_statuses() {
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
            let card = build_block_card_model(&plan.blocks[0], &plan, now_at("10:00"));
            assert_eq!(card.countdown, label, "status={status}");
        }
    }

    #[test]
    fn block_card_expect_by_breach() {
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
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("10:00"));
        assert!(card.has_expect_by_breach);
    }

    #[test]
    fn block_card_expect_by_no_breach() {
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
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("09:30"));
        assert!(!card.has_expect_by_breach);
    }

    #[test]
    fn block_card_expect_by_absent() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus"
start = "09:00"
duration = "30m"
"#,
        );
        let card = build_block_card_model(&plan.blocks[0], &plan, now_at("09:30"));
        assert!(!card.has_expect_by_breach);
    }

    // ── build_today_model ──────────────────────────────────────────────────────

    #[test]
    fn today_model_empty_plan() {
        let plan = Plan::from_toml(PLAN_BASE).unwrap();
        let model = build_today_model(&plan, now_at("10:00"));
        assert_eq!(model.date_label, "2026-06-08");
        assert_eq!(model.now_label, "10:00");
        assert!(model.cards.is_empty());
        assert!(model.now_line_index.is_none());
    }

    #[test]
    fn today_model_now_line_index_before_first_upcoming() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "past-1"
title = "Past"
start = "08:00"
duration = "30m"

[[block]]
id = "future-1"
title = "Future"
start = "12:00"
duration = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("10:00"));
        // "past-1" countdown = "ended", "future-1" countdown starts with "in " → index 1
        assert_eq!(model.now_line_index, Some(1));
    }

    #[test]
    fn today_model_now_line_none_when_all_past() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "past-1"
title = "Past"
start = "08:00"
duration = "30m"
"#,
        );
        let model = build_today_model(&plan, now_at("10:00"));
        assert!(model.now_line_index.is_none());
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
        assert_eq!(model.items[0].reason, None);
    }

    #[test]
    fn approvals_model_reason_from_agent_field() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Automation"
start = "09:00"
duration = "30m"
run = ["/usr/bin/sync"]
agent = "alpha"
"#,
        );
        let model = build_approvals_model(&plan);
        assert_eq!(
            model.items[0].reason,
            Some("Scheduled by: alpha".to_owned())
        );
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
        assert_eq!(model.items[0].kind, ActivityKind::Error);
        assert_eq!(model.items[1].kind, ActivityKind::Ok);
    }

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

    // ── build_upcoming_model ───────────────────────────────────────────────────

    #[test]
    fn upcoming_model_empty_slice() {
        let model = build_upcoming_model(&[], now_at("10:00"));
        assert!(model.days.is_empty());
    }

    #[test]
    fn upcoming_model_single_plan_no_blocks() {
        let plan = Plan::from_toml(PLAN_BASE).unwrap();
        let model = build_upcoming_model(&[plan], now_at("10:00"));
        assert_eq!(model.days.len(), 1);
        assert_eq!(model.days[0].date_label, "2026-06-08");
        assert!(model.days[0].cards.is_empty());
    }

    #[test]
    fn upcoming_model_cards_per_day() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "standup-1"
title = "Standup"
start = "09:00"
duration = "15m"
"#,
        );
        let model = build_upcoming_model(&[plan], now_at("07:00"));
        assert_eq!(model.days[0].cards.len(), 1);
        assert_eq!(model.days[0].cards[0].title, "Standup");
    }

    // ── build_automations_model ────────────────────────────────────────────────

    #[test]
    fn automations_model_empty_rules() {
        let rules = RecurringRules::default();
        let model = build_automations_model(&rules);
        assert!(model.rules.is_empty());
    }

    #[test]
    fn automations_model_daily_rule() {
        let rules = RecurringRules::from_toml(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
"#,
        )
        .unwrap();
        let model = build_automations_model(&rules);
        assert_eq!(model.rules[0].schedule, "daily");
        assert_eq!(model.rules[0].end_label, None);
        assert!(!model.rules[0].has_run);
        assert!(!model.rules[0].has_agent);
    }

    #[test]
    fn automations_model_with_until_end() {
        let rules = RecurringRules::from_toml(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
until = "2026-12-31"
"#,
        )
        .unwrap();
        let model = build_automations_model(&rules);
        assert_eq!(
            model.rules[0].end_label,
            Some("until 2026-12-31".to_owned())
        );
    }

    #[test]
    fn automations_model_with_count_end() {
        let rules = RecurringRules::from_toml(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
count = 10
"#,
        )
        .unwrap();
        let model = build_automations_model(&rules);
        assert_eq!(model.rules[0].end_label, Some("10 occurrences".to_owned()));
    }

    #[test]
    fn automations_model_run_and_agent_flags() {
        let rules = RecurringRules::from_toml(
            r#"
[[block]]
id = "auto"
title = "Auto"
start = "09:00"
duration = "30m"
every = "weekday"
anchor = "2026-06-08"
run = ["/usr/bin/true"]
approval = "approved"
agent = "alpha"
"#,
        )
        .unwrap();
        let model = build_automations_model(&rules);
        assert!(model.rules[0].has_run);
        assert!(model.rules[0].has_agent);
    }

    #[test]
    fn automations_model_no_recurrence_block() {
        // A block without every= ends up with schedule "(no schedule)"
        let rules = RecurringRules::from_toml(
            r#"
[[block]]
id = "manual"
title = "Manual"
start = "09:00"
duration = "30m"
"#,
        )
        .unwrap();
        let model = build_automations_model(&rules);
        assert_eq!(model.rules[0].schedule, "(no schedule)");
        assert_eq!(model.rules[0].end_label, None);
    }

    // ── build_agents_model ─────────────────────────────────────────────────────

    #[test]
    fn agents_model_empty_when_no_agent_records() {
        let model = build_agents_model(&[make_record("notify", "")]);
        assert!(model.agents.is_empty());
    }

    #[test]
    fn agents_model_lists_agents() {
        let r = make_agent_record("alpha", "notify");
        let model = build_agents_model(&[r]);
        assert_eq!(model.agents.len(), 1);
        assert_eq!(model.agents[0].name, "alpha");
        assert!(model.agents[0].is_ok);
    }

    #[test]
    fn agents_model_missed_outcome_is_not_ok() {
        let r = make_agent_record("beta", "missed");
        let model = build_agents_model(&[r]);
        assert!(!model.agents[0].is_ok);
    }

    #[test]
    fn agents_model_failed_outcome_is_not_ok() {
        let r = make_agent_record("gamma", "failed");
        let model = build_agents_model(&[r]);
        assert!(!model.agents[0].is_ok);
    }

    #[test]
    fn agents_model_most_recent_record_wins() {
        let r1 = make_agent_record("alpha", "missed"); // older
        let mut r2 = make_agent_record("alpha", "notify"); // newer
        r2.ts = "2026-06-08T10:00:00Z".parse().unwrap();
        let model = build_agents_model(&[r1, r2]);
        assert_eq!(model.agents.len(), 1);
        assert!(model.agents[0].is_ok); // last record wins
    }

    #[test]
    fn agents_model_sorted_by_name() {
        let r_b = make_agent_record("beta", "notify");
        let r_a = make_agent_record("alpha", "notify");
        let model = build_agents_model(&[r_b, r_a]);
        assert_eq!(model.agents[0].name, "alpha");
        assert_eq!(model.agents[1].name, "beta");
    }

    // ── build_block_detail_model ───────────────────────────────────────────────

    #[test]
    fn block_detail_basic_fields() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "focus-1"
title = "Focus time"
start = "09:00"
end = "09:45"
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.id, "focus-1");
        assert_eq!(model.title, "Focus time");
        assert_eq!(model.time_range, "09:00–09:45");
        assert_eq!(model.recurrence_label, None);
        assert_eq!(model.run_argv, None);
        assert!(model.after_ids.is_empty());
        assert_eq!(model.agent, None);
        assert_eq!(model.approval, None);
    }

    #[test]
    fn block_detail_recurrence_daily() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.recurrence_label, Some("daily".to_owned()));
    }

    #[test]
    fn block_detail_recurrence_with_until() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
until = "2026-12-31"
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(
            model.recurrence_label,
            Some("daily, until 2026-12-31".to_owned())
        );
    }

    #[test]
    fn block_detail_recurrence_with_count() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "standup"
title = "Standup"
start = "09:00"
duration = "15m"
every = "daily"
anchor = "2026-06-08"
count = 5
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.recurrence_label, Some("daily, 5×".to_owned()));
    }

    #[test]
    fn block_detail_run_argv() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Auto"
start = "09:00"
duration = "30m"
run = ["/usr/bin/sync", "--fast"]
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.run_argv, Some("/usr/bin/sync --fast".to_owned()));
        assert_eq!(model.approval, Some("pending".to_owned()));
    }

    #[test]
    fn block_detail_approved_run() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "auto-1"
title = "Auto"
start = "09:00"
duration = "30m"
run = ["/usr/bin/true"]
approval = "approved"
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.approval, Some("approved".to_owned()));
    }

    #[test]
    fn block_detail_agent_field() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "task-1"
title = "Task"
start = "09:00"
duration = "30m"
agent = "my-agent"
"#,
        );
        let model = build_block_detail_model(&plan.blocks[0], &plan, now_at("08:00"));
        assert_eq!(model.agent, Some("my-agent".to_owned()));
    }

    // ── build_up_next_model ────────────────────────────────────────────────────

    #[test]
    fn up_next_empty_plan() {
        let plan = Plan::from_toml(PLAN_BASE).unwrap();
        let model = build_up_next_model(&plan, now_at("10:00"));
        assert!(model.items.is_empty());
    }

    #[test]
    fn up_next_skips_past_and_terminal_blocks() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "past-1"
title = "Past"
start = "08:00"
duration = "30m"
status = "done"

[[block]]
id = "future-1"
title = "Future"
start = "12:00"
duration = "30m"
"#,
        );
        let model = build_up_next_model(&plan, now_at("10:00"));
        assert_eq!(model.items.len(), 1);
        assert_eq!(model.items[0].title, "Future");
    }

    #[test]
    fn up_next_returns_at_most_three() {
        let plan = plan_with_block(
            r#"
[[block]]
id = "a"
title = "A"
start = "11:00"
duration = "30m"

[[block]]
id = "b"
title = "B"
start = "12:00"
duration = "30m"

[[block]]
id = "c"
title = "C"
start = "13:00"
duration = "30m"

[[block]]
id = "d"
title = "D"
start = "14:00"
duration = "30m"
"#,
        );
        let model = build_up_next_model(&plan, now_at("10:00"));
        assert_eq!(model.items.len(), 3);
    }
}
