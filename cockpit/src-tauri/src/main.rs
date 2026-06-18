// ccplan Cockpit — native desktop app over the ccplan engine.
//
// The backend is deliberately thin: reads go through the pure `ccplan::gui::model`
// builders (the same view-models the engine already unit-tests), and every mutation
// is funnelled through `ccplan::run` — the exact same entrypoint the `ccplan` CLI
// uses — so all domain invariants (apply/lead, terminal history, snooze-within-day)
// are reused verbatim rather than re-implemented.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;
use jiff::{Timestamp, Zoned};
use serde::Serialize;

use ccplan::{
    cli::Cli,
    gui::model::{
        ActivityModel, AgentsModel, ApprovalsModel, AutomationsModel, TodayModel, UpNextModel,
        UpcomingModel, build_activity_model, build_agents_model, build_approvals_model,
        build_automations_model, build_today_model, build_up_next_model, build_upcoming_model,
    },
    model::{Plan, PlanDate},
    store::Store,
};

/// Everything one screen needs, in a single round-trip.
#[derive(Serialize)]
struct Snapshot {
    date: String,
    is_today: bool,
    today: TodayModel,
    up_next: UpNextModel,
    approvals: ApprovalsModel,
    automations: AutomationsModel,
    activity: ActivityModel,
    agents: AgentsModel,
    upcoming: UpcomingModel,
    pending_approvals: usize,
}

fn open_store() -> Result<Store, String> {
    match std::env::var_os("CCPLAN_ROOT") {
        Some(root) => Ok(Store::new(&PathBuf::from(root))),
        None => Store::for_user().map_err(|e| e.to_string()),
    }
}

fn today_date() -> PlanDate {
    PlanDate::from_jiff_date(Zoned::now().date())
}

fn resolve_date(date: Option<&str>) -> Result<PlanDate, String> {
    match date {
        Some(s) => s.parse().map_err(|_| format!("invalid date: {s}")),
        None => Ok(today_date()),
    }
}

/// Loads the plan for `date`, or an empty (valid) plan in the system timezone when
/// no plan file exists yet — so the timeline renders an honest empty state.
fn load_or_empty(store: &Store, date: &PlanDate) -> Result<Plan, String> {
    if let Some(plan) = store.load_plan(date).map_err(|e| e.to_string())? {
        return Ok(plan);
    }
    let tz = jiff::tz::TimeZone::system();
    let tz_name = tz.iana_name().unwrap_or("UTC");
    let toml = format!("date = \"{date}\"\ntimezone = \"{tz_name}\"\n");
    Plan::from_toml(&toml).map_err(|e| e.to_string())
}

/// Runs a ccplan CLI invocation in-process, returning its stdout.
fn cli_exec(args: &[String]) -> Result<String, String> {
    let mut full: Vec<String> = Vec::with_capacity(args.len() + 1);
    full.push("ccplan".to_owned());
    full.extend_from_slice(args);
    let cli = Cli::try_parse_from(&full).map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    ccplan::run(cli, &mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn build_snapshot(date: Option<String>) -> Result<Snapshot, String> {
    let store = open_store()?;
    let now = Timestamp::now();
    let date = resolve_date(date.as_deref())?;
    let plan = load_or_empty(&store, &date)?;
    let rules = store.load_recurring_rules().map_err(|e| e.to_string())?;
    let fires = store.read_fire_log().map_err(|e| e.to_string())?;

    // Upcoming: scan a fortnight forward and keep the days that actually have a plan.
    let mut plans: Vec<Plan> = Vec::new();
    let mut cursor = date.as_jiff_date();
    for _ in 0..14 {
        let pd = PlanDate::from_jiff_date(cursor);
        if let Some(p) = store.load_plan(&pd).map_err(|e| e.to_string())? {
            plans.push(p);
        }
        match cursor.tomorrow() {
            Ok(next) => cursor = next,
            Err(_) => break,
        }
    }

    let approvals = build_approvals_model(&plan);
    let pending_approvals = approvals.items.len();

    Ok(Snapshot {
        date: date.to_string(),
        is_today: date == today_date(),
        today: build_today_model(&plan, now),
        up_next: build_up_next_model(&plan, now),
        approvals,
        automations: build_automations_model(&rules),
        activity: build_activity_model(&fires),
        agents: build_agents_model(&fires),
        upcoming: build_upcoming_model(&plans, now),
        pending_approvals,
    })
}

fn opt_date_args(args: &mut Vec<String>, date: &Option<String>) {
    if let Some(d) = date {
        args.push("--date".to_owned());
        args.push(d.clone());
    }
}

#[tauri::command]
fn snapshot(date: Option<String>) -> Result<Snapshot, String> {
    build_snapshot(date)
}

#[tauri::command]
fn add_block(
    date: Option<String>,
    title: String,
    start: String,
    duration: Option<String>,
    end: Option<String>,
    tags: Vec<String>,
) -> Result<Snapshot, String> {
    let mut args = vec![
        "add".to_owned(),
        "--title".to_owned(),
        title,
        "--start".to_owned(),
        start,
    ];
    opt_date_args(&mut args, &date);
    if let Some(dur) = duration {
        args.push("--duration".to_owned());
        args.push(dur);
    } else if let Some(e) = end {
        args.push("--end".to_owned());
        args.push(e);
    }
    if !tags.is_empty() {
        args.push("--tags".to_owned());
        args.push(tags.join(","));
    }
    cli_exec(&args)?;
    cli_exec(&["apply".to_owned()])?;
    build_snapshot(date)
}

#[tauri::command]
fn mark_block(id: String, action: String, date: Option<String>) -> Result<Snapshot, String> {
    let verb = match action.as_str() {
        "done" => "done",
        "skip" => "skip",
        other => return Err(format!("unknown action: {other}")),
    };
    cli_exec(&[verb.to_owned(), id])?;
    build_snapshot(date)
}

#[tauri::command]
fn remove_block(id: String, date: Option<String>) -> Result<Snapshot, String> {
    cli_exec(&["rm".to_owned(), id])?;
    cli_exec(&["apply".to_owned()])?;
    build_snapshot(date)
}

#[tauri::command]
fn snooze_block(id: String, by: String, date: Option<String>) -> Result<Snapshot, String> {
    let mut args = vec!["snooze".to_owned(), id, "--by".to_owned(), by];
    opt_date_args(&mut args, &date);
    cli_exec(&args)?;
    build_snapshot(date)
}

#[tauri::command]
fn approve_block(id: String, date: Option<String>) -> Result<Snapshot, String> {
    let mut args = vec!["approve".to_owned(), id];
    opt_date_args(&mut args, &date);
    cli_exec(&args)?;
    cli_exec(&["apply".to_owned()])?;
    build_snapshot(date)
}

#[tauri::command]
fn apply_triggers(date: Option<String>) -> Result<Snapshot, String> {
    cli_exec(&["apply".to_owned()])?;
    build_snapshot(date)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            snapshot,
            add_block,
            mark_block,
            remove_block,
            snooze_block,
            approve_block,
            apply_triggers,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ccplan Cockpit");
}
