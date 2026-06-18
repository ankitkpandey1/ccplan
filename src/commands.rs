//! Command dispatch and platform-agnostic command behavior.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use clap::CommandFactory;
use clap_complete::shells::{Bash, Fish, PowerShell, Zsh};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::{
    cli::{
        AddArgs, AgendaArgs, ApplyArgs, ApproveArgs, ClearArgs, Cli, Commands, DiffArgs, EditArgs,
        FireArgs, LogArgs, MaterializeArgs, ReadArgs, RemindArgs, ServeArgs, SetArgs, Shell,
        SnoozeArgs, TemplateApplyArgs, TemplateArgs, TemplateCommand, TemplateNameArgs, WatchArgs,
    },
    config::AutomationConfig,
    context::{ContextRefs, Notification, Scheduler},
    error::{Error, Result},
    lifecycle::{Event, FireDecision, awaiting_approval, decide_fire, reconcile_overdue},
    model::{
        Approval, Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, RecurEnd,
        Recurrence, Retry, Run, ScheduleRev, Span, Status, TimeZoneName, WhenCondition,
    },
    serve::{
        AgentAssignment, AgentEventKey, ConditionState, ServeMemory, decide_agent_assignments,
        decide_reactive_triggers,
    },
    store::{
        FireRecord, FiredEventKey, FiredStatus, HistoryPolicy, Store, TriggerKind, TriggerRecord,
    },
    time::{resolve_block_end, resolve_block_start},
};

pub fn dispatch(
    command: Option<Commands>,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    match command {
        None => Ok(()),
        Some(Commands::Set(args)) => set(args, out, context),
        Some(Commands::Add(args)) => add(args, context),
        Some(Commands::Remind(args)) => remind(args, out, context),
        Some(Commands::Edit(args)) => edit(args, context),
        Some(Commands::Rm(args)) => remove(&args.id, context),
        Some(Commands::Done(args)) => set_status(args.id, Status::Done, context),
        Some(Commands::Skip(args)) => set_status(args.id, Status::Skipped, context),
        Some(Commands::Snooze(args)) => snooze(args, out, context),
        Some(Commands::Clear(args)) => clear(args, out, context),
        Some(Commands::Show(args)) => show(args, out, context),
        Some(Commands::Now(args)) => now(args, out, context),
        Some(Commands::Next(args)) => next(args, out, context),
        Some(Commands::Agenda(args)) => agenda(args, out, context),
        Some(Commands::Watch(args)) => watch(args, out, context),
        Some(Commands::Serve(args)) => serve(args, out, context),
        Some(Commands::Apply(args)) => apply(args, out, context),
        Some(Commands::Diff(args)) => diff(args, out, context),
        Some(Commands::Approve(args)) => approve(args, out, context),
        Some(Commands::Materialize(args)) => materialize(&args, out, context),
        Some(Commands::Fire(args)) => fire(&args, out, context),
        Some(Commands::Roll) => roll(out, context),
        Some(Commands::Log(args)) => fire_log(args, out, context),
        Some(Commands::Template(args)) => template(args, out, context),
        Some(Commands::Status) => status(out, context),
        Some(Commands::Doctor) => doctor(out, context),
        Some(Commands::Completions(args)) => {
            completions(args.shell, out);
            Ok(())
        }
        Some(Commands::Mcp(_args)) => crate::mcp::run_mcp_server(context),
        Some(Commands::Gui) => crate::gui::launch_cockpit(),
    }
}

fn set(args: SetArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let input = read_plan_input(&args.from)?;
    set_from_str(&input, args.date, args.override_history, out, context)
}

pub(crate) fn set_from_str(
    input: &str,
    date: Option<PlanDate>,
    override_history: bool,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    let mut plan = Plan::from_toml_with_default(input, context.config.notify.default_lead)?;
    if let Some(date) = date {
        plan.date = date;
    }
    let policy = if override_history {
        HistoryPolicy::Override
    } else {
        HistoryPolicy::Preserve
    };
    let stored =
        context
            .store
            .set_plan_with_default(&plan, policy, context.config.notify.default_lead)?;
    writeln!(out, "stored {}", stored.date)?;
    Ok(())
}

fn recurrence_from_add_args(
    date: &PlanDate,
    every: Option<String>,
    until: Option<PlanDate>,
    count: Option<u32>,
) -> Result<Option<Recurrence>> {
    let Some(every) = every else {
        if until.is_some() || count.is_some() {
            return Err(Error::Usage(
                "`--until` and `--count` require `--every`".to_owned(),
            ));
        }
        return Ok(None);
    };
    if until.is_some() && count.is_some() {
        return Err(Error::Usage(
            "`--until` and `--count` are mutually exclusive".to_owned(),
        ));
    }
    let rule = crate::recurrence::parse_every(&every).map_err(Error::from)?;
    let end = until
        .map(RecurEnd::Until)
        .or_else(|| count.map(RecurEnd::Count));
    Ok(Some(Recurrence {
        rule,
        anchor: date.clone(),
        end,
    }))
}

fn retry_from_add_arg(raw: Option<String>) -> Result<Option<Retry>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (count_raw, backoff_raw) = raw
        .split_once(':')
        .ok_or_else(|| Error::Usage("`--retry` must use COUNT:BACKOFF, e.g. `3:30s`".to_owned()))?;
    if count_raw.is_empty() || backoff_raw.is_empty() {
        return Err(Error::Usage(
            "`--retry` must use COUNT:BACKOFF, e.g. `3:30s`".to_owned(),
        ));
    }
    let count = count_raw
        .parse::<u32>()
        .map_err(|_| Error::Usage("`--retry` count must be an unsigned integer".to_owned()))?;
    let backoff = backoff_raw.parse::<DurationSpec>().map_err(Error::from)?;
    Ok(Some(Retry { count, backoff }))
}

fn add(args: AddArgs, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let id = match args.id {
        Some(id) => id,
        None => slug_block_id(&args.title)?,
    };
    let run = run_from(args.run)?;
    let recurrence = recurrence_from_add_args(&date, args.every, args.until, args.count)?;
    let retry = retry_from_add_arg(args.retry)?;
    let approval = if run.is_some() {
        Some(crate::model::Approval::Pending)
    } else {
        None
    };
    let block = Block {
        id: id.clone(),
        title: args.title,
        start: args.start,
        span: span_from(args.end, args.duration)?,
        notify: args.notify.unwrap_or(context.config.notify.default_lead),
        tags: args.tags,
        status: Status::Pending,
        run,
        recurrence,
        origin: None,
        after: args.after,
        on_success: vec![],
        on_failure: vec![],
        on_missed: vec![],
        retry,
        expect_by: args.expect_by,
        approval,
        when: None,
        agent: None,
    };

    // The whole load→mutate→write runs under the store lock (Inv-17), so a concurrent writer
    // adding a different block to the same day cannot be lost.
    update_plan(context, &date, |existing| {
        let mut plan = match existing {
            Some(plan) => plan,
            None => empty_plan(date.clone(), timezone_from_clock(context)?),
        };
        match plan.blocks.iter().position(|existing| existing.id == id) {
            Some(index) if plan.blocks[index].status.is_terminal() => {
                return Err(Error::HistoryConflict { id });
            }
            Some(index) => plan.blocks[index] = block,
            None => plan.blocks.push(block),
        }
        Ok(plan)
    })
}

/// Minute-granular window for a one-shot reminder block.
///
/// A reminder is a point in time, but a block needs a span; one minute is long enough to give the
/// block a well-formed `end` event and short enough that it leaves the agenda promptly after firing.
/// `from_seconds_const(60)` is always `Some` (60 is within a single day); the `match` is the
/// const-context idiom (mirrors `config::DEFAULT_GRACE`).
const REMINDER_WINDOW: DurationSpec = match DurationSpec::from_seconds_const(60) {
    Some(window) => window,
    None => unreachable!(),
};

/// Sets a one-shot reminder `--in <duration>` from now, then applies it immediately.
///
/// Sugar over `add` + `apply`: it resolves the absolute wall-clock target in the clock's time zone
/// (minute granularity, like the rest of the system), creates a zero-lead block so the only alert is
/// the `start` event firing exactly at the target (the heads-up `notify` trigger is omitted when the
/// lead is zero, Inv-16; the `start` event always notifies, DESIGN §6.3), and auto-applies so the OS
/// trigger goes live without a second command.
fn remind(args: RemindArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let RemindArgs { text, fire_in, id } = args;
    let lead = SignedDuration::from_secs(i64::from(fire_in.as_seconds()));
    let target = context
        .clock
        .now()
        .checked_add(lead)
        .map_err(crate::time::TimeError::from)?;
    let date = PlanDate::from_jiff_date(target.date());
    let minutes =
        u16::from(target.hour().unsigned_abs()) * 60 + u16::from(target.minute().unsigned_abs());
    let start = ClockTime::from_minutes_since_midnight(minutes).map_err(Error::from)?;

    let id = match id {
        Some(id) => id,
        None => slug_block_id(&text)?,
    };
    let block = Block {
        id: id.clone(),
        title: text.clone(),
        start,
        span: Span::Duration(REMINDER_WINDOW),
        notify: Lead::from_seconds_const(0),
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

    // Same transactional load→mutate→write as `add` (Inv-17): a concurrent writer on the same day
    // can't be lost, and a colliding terminal block is refused rather than overwritten.
    update_plan(context, &date, |existing| {
        let mut plan = match existing {
            Some(plan) => plan,
            None => empty_plan(date.clone(), timezone_from_clock(context)?),
        };
        match plan.blocks.iter().position(|existing| existing.id == id) {
            Some(index) if plan.blocks[index].status.is_terminal() => {
                Err(Error::HistoryConflict { id })
            }
            Some(index) => {
                plan.blocks[index] = block;
                Ok(plan)
            }
            None => {
                plan.blocks.push(block);
                Ok(plan)
            }
        }
    })?;

    writeln!(out, "reminder \"{text}\" set for {start} on {date}")?;
    apply(
        ApplyArgs {
            date: Some(date),
            dry_run: false,
        },
        out,
        context,
    )
}

fn edit(args: EditArgs, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    if args.end.is_some() && args.duration.is_some() {
        return Err(Error::Usage(
            "edit accepts only one of --end or --duration".to_owned(),
        ));
    }

    update_plan(context, &date, |existing| {
        let mut plan = required_plan(existing, &date)?;
        let block = find_block_mut(&mut plan, &args.id)?;
        ensure_non_terminal(block)?;

        if let Some(title) = args.title {
            block.title = title;
        }
        if let Some(start) = args.start {
            block.start = start;
        }
        if let Some(end) = args.end {
            block.span = Span::End(end);
        }
        if let Some(duration) = args.duration {
            block.span = Span::Duration(duration);
        }
        if let Some(notify) = args.notify {
            block.notify = notify;
        }
        if !args.run.is_empty() {
            block.run = Some(Run::new(args.run)?);
        }
        Ok(plan)
    })
}

fn remove(id: &BlockId, context: &ContextRefs<'_>) -> Result<()> {
    let date = today(context);
    update_plan(context, &date, |existing| {
        let mut plan = required_plan(existing, &date)?;
        let index = plan
            .blocks
            .iter()
            .position(|block| &block.id == id)
            .ok_or_else(|| Error::NotFound(format!("block `{id}`")))?;
        ensure_non_terminal(&plan.blocks[index])?;
        plan.blocks.remove(index);
        Ok(plan)
    })
}

fn set_status(id: BlockId, status: Status, context: &ContextRefs<'_>) -> Result<()> {
    let date = today(context);
    update_plan(context, &date, |existing| {
        let mut plan = required_plan(existing, &date)?;
        let block = find_block_mut(&mut plan, &id)?;
        if block.status.is_terminal() && block.status != status {
            return Err(Error::HistoryConflict { id });
        }
        block.status = status;
        Ok(plan)
    })
}

/// Pushes a non-terminal block later by a duration, then re-applies so OS triggers track the slide.
///
/// Sliding `start` (and, for an absolute-`end` span, `end` too, preserving the block's length) changes
/// the block's `schedule_rev`, so `apply` reconciles the stale triggers for the old time away and arms
/// the new ones in one command. Per NG8 (no rollover) a snooze that would cross midnight is refused
/// rather than silently wrapping into the next day.
fn snooze(args: SnoozeArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let SnoozeArgs { id, by, date } = args;
    let date = date.unwrap_or_else(|| today(context));
    let by_minutes = by.as_seconds() / 60;
    update_plan(context, &date, |existing| {
        let mut plan = required_plan(existing, &date)?;
        let block = find_block_mut(&mut plan, &id)?;
        ensure_non_terminal(block)?;
        block.start = snooze_clock(block.start, by_minutes)?;
        if let Span::End(end) = block.span {
            block.span = Span::End(snooze_clock(end, by_minutes)?);
        }
        Ok(plan)
    })?;
    writeln!(out, "snoozed {id} by {by} on {date}")?;
    apply(
        ApplyArgs {
            date: Some(date),
            dry_run: false,
        },
        out,
        context,
    )
}

/// Shifts a wall-clock time `by_minutes` later, refusing a slide that would leave the day (Inv: NG8).
fn snooze_clock(time: ClockTime, by_minutes: u32) -> Result<ClockTime> {
    let shifted = u32::from(time.minutes_since_midnight()) + by_minutes;
    u16::try_from(shifted)
        .ok()
        .and_then(|minutes| ClockTime::from_minutes_since_midnight(minutes).ok())
        .ok_or_else(|| {
            Error::Usage(format!(
                "snooze would move {time} past midnight; keep the block within the same day"
            ))
        })
}

/// Dispatches the `template` subcommands (save / list / apply).
fn template(args: TemplateArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    match args.command {
        TemplateCommand::Save(name_args) => template_save(name_args, out, context),
        TemplateCommand::List => template_list(out, context),
        TemplateCommand::Apply(name_args) => template_apply(name_args, out, context),
    }
}

/// Saves the plan for a date as a named, reusable template (its plain TOML).
fn template_save(
    args: TemplateNameArgs,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    let name = validate_template_name(&args.name)?;
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = load_required(context.store, &date, context.config.notify.default_lead)?;
    context.store.save_template(&name, &plan.to_toml()?)?;
    writeln!(out, "saved template {name} from {date}")?;
    Ok(())
}

/// Lists saved template names, one per line (or a plain-language line when none exist).
fn template_list(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let names = context.store.list_templates()?;
    if names.is_empty() {
        writeln!(out, "no templates saved")?;
    } else {
        for name in names {
            writeln!(out, "{name}")?;
        }
    }
    Ok(())
}

/// Instantiates a template onto a date and applies it.
///
/// The stored shape is stamped with the target date and every block is reset to `pending`, so a
/// template captured from a lived-in day starts fresh. Persisting uses the preserve-history policy
/// like `set`, so instantiating over a day that already holds terminal blocks is refused (exit 6)
/// rather than silently erasing history.
fn template_apply(
    args: TemplateApplyArgs,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    let name = validate_template_name(&args.name)?;
    let date = args.date.unwrap_or_else(|| today(context));
    let toml = context
        .store
        .load_template(&name)?
        .ok_or_else(|| Error::NotFound(format!("template `{name}`")))?;
    let vars = parse_template_vars(&args.vars)?;
    let toml = substitute_template_vars(&toml, &vars)?;
    let mut plan = Plan::from_toml_with_default(&toml, context.config.notify.default_lead)?;
    plan.date = date.clone();
    for block in &mut plan.blocks {
        block.status = Status::Pending;
    }
    persist_plan(context, &plan)?;
    writeln!(out, "applied template {name} to {date}")?;
    apply(
        ApplyArgs {
            date: Some(date),
            dry_run: false,
        },
        out,
        context,
    )
}

/// Validates a template name is a safe slug: non-empty, only ASCII letters, digits, `-` or `_`.
///
/// This is the path-traversal guard — rejecting `/`, `.`, and `..` keeps `template_path` from
/// escaping the templates directory.
fn validate_template_name(name: &str) -> Result<String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        Ok(name.to_owned())
    } else {
        Err(Error::Usage(format!(
            "template name must be non-empty and use only letters, digits, '-' or '_': {name:?}"
        )))
    }
}

fn parse_template_vars(raw_vars: &[String]) -> Result<BTreeMap<String, String>> {
    let mut vars = BTreeMap::new();
    for raw in raw_vars {
        let Some((name, value)) = raw.split_once('=') else {
            return Err(Error::Usage(format!(
                "template variable must be NAME=VALUE: {raw:?}"
            )));
        };
        let name = validate_template_name(name)?;
        vars.insert(name, value.to_owned());
    }
    Ok(vars)
}

fn substitute_template_vars(input: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            return Err(Error::Usage(
                "unterminated template variable; expected `}`".to_owned(),
            ));
        };
        let name = validate_template_name(&after_open[..end])?;
        let value = vars.get(&name).ok_or_else(|| {
            Error::Usage(format!(
                "missing template variable `{name}`; pass --var {name}=VALUE"
            ))
        })?;
        output.push_str(value);
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn clear(args: ClearArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    if !args.yes {
        return Err(Error::Usage("clear requires --yes".to_owned()));
    }

    let date = args.date.unwrap_or_else(|| today(context));
    let changes = reconcile_triggers(context.store, context.scheduler, &date, &[], args.dry_run)?;
    write_reconcile_summary(out, &changes)?;
    if !args.dry_run {
        if args.purge {
            context.store.purge(&date)?;
        } else {
            context.store.archive(&date)?;
        }
    }
    Ok(())
}

fn show(args: ReadArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = load_required(context.store, &date, context.config.notify.default_lead)?;
    if args.json {
        serde_json::to_writer_pretty(&mut *out, &plan)?;
        writeln!(out)?;
    } else {
        write!(out, "{}", plan.to_toml()?)?;
    }
    Ok(())
}

fn now(args: ReadArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = read_reconciled_plan(context, &date)?;
    let now = context.clock.now().timestamp();
    let mut blocks = Vec::new();
    for block in &plan.blocks {
        if block.status.is_terminal() {
            continue;
        }
        let start = resolve_block_start(&plan, block)?;
        let end = resolve_block_end(&plan, block)?;
        if start <= now && now < end {
            blocks.push(BlockSummary::from_block(block));
        }
    }
    write_read_rows(out, args.json, &blocks, "no active blocks right now")
}

fn next(args: ReadArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = read_reconciled_plan(context, &date)?;
    let now = context.clock.now().timestamp();
    let mut candidates = Vec::new();
    for block in &plan.blocks {
        if block.status.is_terminal() {
            continue;
        }
        let start = resolve_block_start(&plan, block)?;
        if start > now {
            candidates.push((start, BlockSummary::from_block(block)));
        }
    }
    let Some(next_start) = candidates.iter().map(|(start, _)| *start).min() else {
        return write_read_rows(
            out,
            args.json,
            &Vec::<BlockSummary>::new(),
            "no upcoming blocks today",
        );
    };
    let blocks = candidates
        .into_iter()
        .filter_map(|(start, block)| (start == next_start).then_some(block))
        .collect::<Vec<_>>();
    write_read_rows(out, args.json, &blocks, "no upcoming blocks today")
}

fn agenda(args: AgendaArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = read_reconciled_plan(context, &date)?;
    let now = context.clock.now().timestamp();
    let mut blocks = Vec::new();
    for block in &plan.blocks {
        if block.status.is_terminal() {
            continue;
        }
        let end = resolve_block_end(&plan, block)?;
        if end <= now {
            continue;
        }
        let start = resolve_block_start(&plan, block)?;
        let starts_in_seconds = start.duration_since(now).as_secs();
        blocks.push(AgendaEntry::new(block, starts_in_seconds));
    }
    write_read_rows(out, args.json, &blocks, "nothing left on today's agenda")
}

/// One terminal-clear escape (`ESC[2J`) plus cursor-home (`ESC[H`) — redraws a watch frame in place.
const WATCH_CLEAR: &str = "\x1b[2J\x1b[H";

/// What the refresh driver tells the [`watch_loop`] to do after a frame is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchSignal {
    /// Redraw the agenda (the timer elapsed).
    Refresh,
    /// Stop watching (the user interrupted, or input/EOF arrived).
    Quit,
}

/// The loop's only side-effecting dependency: blocks until the next [`WatchSignal`]. Real watching
/// waits on a timer and on terminal input (see `RealWatchClock`); tests inject a scripted driver so
/// the loop body stays fully covered without sleeping or a live terminal.
trait WatchClock {
    fn wait(&mut self) -> WatchSignal;
}

/// Renders the live agenda, then redraws it each time the driver signals `Refresh`, returning when
/// it signals `Quit`. Pure aside from `out` and the injected driver — fully tested.
fn watch_loop(
    date: Option<PlanDate>,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
    clock: &mut dyn WatchClock,
) -> Result<()> {
    let date = date.unwrap_or_else(|| today(context));
    loop {
        let frame = render_watch_frame(context, &date)?;
        write!(out, "{WATCH_CLEAR}{frame}")?;
        out.flush()?;
        if clock.wait() == WatchSignal::Quit {
            return Ok(());
        }
    }
}

/// Builds one watch frame: a header (date + wall-clock time + quit hint) above the live agenda
/// table, reusing `agenda`'s human rendering verbatim so the two views never drift. Pure (tested).
fn render_watch_frame(context: &ContextRefs<'_>, date: &PlanDate) -> Result<String> {
    let wall = context.clock.now().strftime("%H:%M:%S");
    let mut buf: Vec<u8> = Vec::new();
    writeln!(buf, "ccplan watch · {date} · {wall}")?;
    writeln!(buf, "(Ctrl-C or Enter to quit)")?;
    writeln!(buf)?;
    let args = AgendaArgs {
        date: Some(date.clone()),
        json: false,
    };
    agenda(args, &mut buf, context)?;
    Ok(String::from_utf8(buf).expect("agenda renders valid UTF-8"))
}

/// Drives [`watch_loop`] in production: a background thread reads terminal input while the main
/// thread waits on a channel with the refresh interval as its timeout. A timeout means "redraw";
/// any line, EOF, or a closed channel means "quit". This is the genuine timer/thread/stdin IO
/// boundary — excluded from coverage; the loop logic it feeds lives in the tested `watch_loop`.
struct RealWatchClock {
    interval: std::time::Duration,
    quit: std::sync::mpsc::Receiver<()>,
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl RealWatchClock {
    fn spawn(interval: std::time::Duration) -> Self {
        let (tx, quit) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            // One read is enough: a line, or EOF, both mean "stop watching".
            let _ = std::io::stdin().read_line(&mut line);
            let _ = tx.send(());
        });
        Self { interval, quit }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl WatchClock for RealWatchClock {
    fn wait(&mut self) -> WatchSignal {
        use std::sync::mpsc::RecvTimeoutError;
        match self.quit.recv_timeout(self.interval) {
            Err(RecvTimeoutError::Timeout) => WatchSignal::Refresh,
            Ok(()) | Err(RecvTimeoutError::Disconnected) => WatchSignal::Quit,
        }
    }
}

/// Live, auto-refreshing read-only agenda. Wires the real timer/input driver into `watch_loop`.
#[cfg_attr(coverage_nightly, coverage(off))]
fn watch(args: WatchArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let interval = std::time::Duration::from_secs(u64::from(args.every.as_seconds()));
    let mut clock = RealWatchClock::spawn(interval);
    watch_loop(args.date, out, context, &mut clock)
}

trait ConditionProbe {
    fn state(&self, condition: &WhenCondition) -> Result<ConditionState>;
}

struct RealConditionProbe<'a> {
    automation: &'a AutomationConfig,
}

impl ConditionProbe for RealConditionProbe<'_> {
    fn state(&self, condition: &WhenCondition) -> Result<ConditionState> {
        match condition {
            WhenCondition::FileExists(path) => {
                if Path::new(path).exists() {
                    Ok(ConditionState::satisfied("exists"))
                } else {
                    Ok(ConditionState::unsatisfied())
                }
            }
            WhenCondition::FileChanged(path) => {
                file_changed_state(fs::metadata(path).and_then(|m| m.modified()))
            }
            WhenCondition::CommandOk(argv) => command_ok_state(self.automation, argv),
        }
    }
}

fn command_ok_state(automation: &AutomationConfig, argv: &[String]) -> Result<ConditionState> {
    authorize_run(automation, argv).map_err(Error::AutomationRefused)?;
    if command_ok_status(argv)? {
        Ok(ConditionState::satisfied("ok"))
    } else {
        Ok(ConditionState::unsatisfied())
    }
}

fn command_ok_status(argv: &[String]) -> Result<bool> {
    #[cfg(coverage_nightly)]
    {
        Ok(argv[0].ends_with("true"))
    }
    #[cfg(not(coverage_nightly))]
    {
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()?;
        Ok(status.success())
    }
}

fn file_changed_state(modified: std::io::Result<std::time::SystemTime>) -> Result<ConditionState> {
    match modified {
        Ok(modified) => Ok(ConditionState::satisfied(file_changed_marker(modified))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ConditionState::unsatisfied())
        }
        Err(error) => Err(error.into()),
    }
}

fn file_changed_marker(modified: std::time::SystemTime) -> String {
    let marker = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("mtime:{}:{}", marker.as_secs(), marker.subsec_nanos())
}

fn collect_condition_states(
    plan: &Plan,
    probe: &dyn ConditionProbe,
) -> Result<HashMap<BlockId, ConditionState>> {
    let mut states = HashMap::new();
    for block in &plan.blocks {
        if let Some(condition) = &block.when {
            states.insert(block.id.clone(), probe.state(condition)?);
        }
    }
    Ok(states)
}

fn due_agent_blocks(plan: &Plan, now: Timestamp) -> Result<HashSet<BlockId>> {
    let mut due = HashSet::new();
    for block in &plan.blocks {
        if block.agent.is_none() || block.status.is_terminal() {
            continue;
        }
        let start = resolve_block_start(plan, block)?;
        if start <= now {
            due.insert(block.id.clone());
        }
    }
    Ok(due)
}

fn claimed_agent_events(records: &[FireRecord]) -> Vec<AgentEventKey> {
    records
        .iter()
        .filter_map(|record| {
            let agent = record.agent.as_ref()?;
            Some(AgentEventKey {
                agent: agent.clone(),
                block_id: record.id.clone(),
                event: record.event,
            })
        })
        .collect()
}

fn record_agent_assignment(
    context: &ContextRefs<'_>,
    plan: &Plan,
    assignment: &AgentAssignment,
) -> Result<bool> {
    let block = plan
        .blocks
        .iter()
        .find(|block| block.id == assignment.block_id)
        .expect("agent assignment references a plan block");
    let scheduled_at = resolve_block_start(plan, block)?;
    let key = FiredEventKey {
        date: plan.date.clone(),
        block_id: assignment.block_id.clone(),
        event: assignment.event,
        rev: block.schedule_rev(),
        scheduled_at,
        attempt: 0,
        agent: Some(assignment.agent.clone()),
    };
    if context.store.check_and_set_fired(key)? == FiredStatus::AlreadyFired {
        return Ok(false);
    }
    let record = FireRecord {
        ts: context.clock.now().timestamp(),
        date: plan.date.clone(),
        id: assignment.block_id.clone(),
        event: assignment.event,
        outcome: "agent".to_owned(),
        detail: format!(
            "agent-assigned agent={}",
            sanitize_log_field(&assignment.agent)
        ),
        agent: Some(assignment.agent.clone()),
    };
    append_fire_record(context.store, &record)?;
    Ok(true)
}

fn serve_tick(
    date: Option<PlanDate>,
    agent: Option<&str>,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
    probe: &dyn ConditionProbe,
    memory: &ServeMemory,
) -> Result<ServeMemory> {
    let date = date.unwrap_or_else(|| today(context));
    let Some(mut plan) = context
        .store
        .load_plan_with_default(&date, context.config.notify.default_lead)?
    else {
        writeln!(out, "serve: no plan for {date}")?;
        return Ok(memory.clone());
    };
    apply_overdue_in_memory(context, &mut plan)?;
    let states = collect_condition_states(&plan, probe)?;
    let tick = decide_reactive_triggers(&plan, &states, memory);
    let now = context.clock.now().timestamp();
    let mut did_work = false;
    for decision in &tick.decisions {
        arm_successor(context, out, &plan, &decision.block_id, now)?;
        writeln!(out, "serve: armed reactive {}", decision.block_id)?;
        did_work = true;
    }
    if tick.decisions.is_empty() {
        writeln!(out, "serve: no reactive triggers")?;
    }
    if let Some(agent) = agent {
        let due = due_agent_blocks(&plan, now)?;
        let records = context.store.read_fire_log()?;
        let claimed = claimed_agent_events(&records);
        let assignments = decide_agent_assignments(&plan, agent, &due, &claimed);
        let mut assigned = 0usize;
        for assignment in &assignments {
            if record_agent_assignment(context, &plan, assignment)? {
                let message = format!(
                    "serve: assigned {} to {}",
                    assignment.block_id, assignment.agent
                );
                writeln!(out, "{message}")?;
                assigned += 1;
                did_work = true;
            }
        }
        if assigned == 0 {
            writeln!(out, "serve: no agent assignments for {agent}")?;
        }
    }
    if !did_work {
        out.flush()?;
    }
    Ok(tick.next_memory)
}

#[cfg(not(coverage_nightly))]
fn serve_loop(
    args: &ServeArgs,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
    probe: &dyn ConditionProbe,
    mut memory: ServeMemory,
) -> Result<()> {
    let interval = std::time::Duration::from_secs(u64::from(args.every.as_seconds()));
    loop {
        memory = serve_tick(
            args.date.clone(),
            args.agent.as_deref(),
            out,
            context,
            probe,
            &memory,
        )?;
        out.flush()?;
        std::thread::sleep(interval);
    }
}

/// Runs the optional resident daemon. Default ccplan remains daemonless unless this is invoked.
#[cfg(coverage_nightly)]
fn serve(args: ServeArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let probe = RealConditionProbe {
        automation: &context.config.automation,
    };
    let memory = ServeMemory::default();
    let date = args.date;
    let agent = args.agent.as_deref();
    let _ = serve_tick(date, agent, out, context, &probe, &memory)?;
    Ok(())
}

/// Runs the optional resident daemon. Default ccplan remains daemonless unless this is invoked.
#[cfg(not(coverage_nightly))]
fn serve(args: ServeArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let probe = RealConditionProbe {
        automation: &context.config.automation,
    };
    let memory = ServeMemory::default();
    if args.once {
        let _ = serve_tick(
            args.date,
            args.agent.as_deref(),
            out,
            context,
            &probe,
            &memory,
        )?;
        Ok(())
    } else {
        serve_loop(&args, out, context, &probe, memory)
    }
}

/// Reads the fire ledger — what the scheduler actually did — newest filters applied.
///
/// The read side of close-the-loop: optionally narrow to one `--date` or to fires at/after a
/// `--since` timestamp, then emit machine `--json` or a scannable human table. A missing ledger is
/// an empty history, not an error.
fn fire_log(args: LogArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let LogArgs { date, since, json } = args;
    let mut records = context.store.read_fire_log()?;
    if let Some(date) = &date {
        records.retain(|record| &record.date == date);
    }
    if let Some(since) = since {
        records.retain(|record| record.ts >= since);
    }
    if json {
        serde_json::to_writer_pretty(&mut *out, &records)?;
        writeln!(out)?;
    } else if records.is_empty() {
        writeln!(out, "no fires recorded")?;
    } else {
        for record in &records {
            let line = format!(
                "{}  {} {} {}  {}  {}",
                record.ts, record.date, record.id, record.event, record.outcome, record.detail
            );
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

fn apply(args: ApplyArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    // `apply` is a mutation point and persists overdue reconciliation; `--dry-run` is a preview and
    // must stay side-effect-free, so it reconciles in memory only (Inv-18).
    let plan = if args.dry_run {
        read_reconciled_plan(context, &date)?
    } else {
        reconciled_plan(context, &date)?
    };
    let desired = desired_triggers(&plan, context.clock.now().timestamp())?;
    let store = context.store;
    let scheduler = context.scheduler;
    if !args.dry_run {
        scheduler.prepare()?;
        if let Err(error) = context.notifier.check() {
            writeln!(out, "warning: notifier: {error}")?;
        }
    }
    let changes = reconcile_triggers(store, scheduler, &date, &desired, args.dry_run)?;
    write_reconcile_summary(out, &changes)
}

fn diff(args: DiffArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    apply(
        ApplyArgs {
            date: args.date,
            dry_run: true,
        },
        out,
        context,
    )
}

fn approve(args: ApproveArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    update_plan(context, &date, |existing| {
        let mut plan = required_plan(existing, &date)?;
        let block = find_block_mut(&mut plan, &args.id)?;
        ensure_non_terminal(block)?;
        if block.run.is_none() {
            return Err(Error::Usage(format!(
                "block `{}` has no run command to approve",
                args.id
            )));
        }
        block.approval = Some(Approval::Approved);
        Ok(plan)
    })?;
    writeln!(out, "approved {} on {date}", args.id)?;
    Ok(())
}

/// Expands recurring templates into concrete dated occurrences for the next `horizon` days.
///
/// For each date in `[today, today+horizon)`, materializes the recurring rules, merges with any
/// existing plan, then calls the existing single-date `apply` path.  `apply` itself stays
/// single-date; this loop is the only horizon-wide caller.
fn materialize(
    args: &MaterializeArgs,
    out: &mut dyn Write,
    context: &ContextRefs<'_>,
) -> Result<()> {
    let today = today(context);
    let default_lead = context.config.notify.default_lead;
    for i in 0..args.horizon {
        let date_jiff = today
            .as_jiff_date()
            .checked_add(jiff::Span::new().days(i64::from(i)))
            .map_err(crate::time::TimeError::from)?;
        let date = PlanDate::from_jiff_date(date_jiff);
        let plan = context.store.materialize_for_date(&date, default_lead)?;
        context
            .store
            .set_plan_with_default(&plan, HistoryPolicy::Preserve, default_lead)?;
        apply(
            ApplyArgs {
                date: Some(date.clone()),
                dry_run: false,
            },
            out,
            context,
        )?;
        writeln!(out, "materialized {date}")?;
    }
    Ok(())
}

/// Re-materializes the far-edge date, applies it, then self-perpetuates by scheduling tomorrow's
/// roll trigger.  A missed roll only shortens the horizon (Persistent=false).
fn roll(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    const ROLL_HORIZON: u32 = 14;
    let today = today(context);
    let default_lead = context.config.notify.default_lead;

    let far_edge_jiff = today
        .as_jiff_date()
        .checked_add(jiff::Span::new().days(i64::from(ROLL_HORIZON - 1)))
        .map_err(crate::time::TimeError::from)?;
    let far_edge = PlanDate::from_jiff_date(far_edge_jiff);
    let plan = context
        .store
        .materialize_for_date(&far_edge, default_lead)?;
    context
        .store
        .set_plan_with_default(&plan, HistoryPolicy::Preserve, default_lead)?;
    apply(
        ApplyArgs {
            date: Some(far_edge.clone()),
            dry_run: false,
        },
        out,
        context,
    )?;
    writeln!(out, "rolled far edge to {far_edge}")?;

    // Schedule tomorrow's roll at 00:05 UTC.
    let tomorrow_jiff = today
        .as_jiff_date()
        .checked_add(jiff::Span::new().days(1))
        .map_err(crate::time::TimeError::from)?;
    let tomorrow = PlanDate::from_jiff_date(tomorrow_jiff);
    let roll_ts = tomorrow_jiff
        .at(0, 5, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(crate::time::TimeError::from)?
        .timestamp();

    let roll_trigger = TriggerRecord {
        backend_id: format!("ccplan-roll-{tomorrow}"),
        date: tomorrow,
        block_id: BlockId::new("ccplan-roll").map_err(Error::from)?,
        event: Event::Start,
        rev: ScheduleRev::new("0000000000000000").map_err(Error::from)?,
        scheduled_at: roll_ts,
        kind: TriggerKind::Roll,
        attempt: 0,
    };
    context.scheduler.prepare()?;
    context.store.record_trigger(roll_trigger.clone())?;
    context.scheduler.add(&roll_trigger)?;
    Ok(())
}

fn fire(args: &FireArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    // The post-fire OS cleanup (macOS launchd self-bootout) is a side effect, so a `--dry-run`
    // preview must not arm it. `then` is lazy, so for a dry run no `FireCleanup` is constructed.
    let _cleanup = (!args.dry_run).then(|| FireCleanup);
    let Some(mut plan) = context
        .store
        .load_plan_with_default(&args.date, context.config.notify.default_lead)?
    else {
        return Ok(());
    };
    let Some(index) = plan.blocks.iter().position(|block| block.id == args.id) else {
        return Ok(());
    };
    if plan.blocks[index].schedule_rev() != args.rev {
        return Ok(());
    }

    let decision = decide_fire(
        &plan.blocks[index],
        args.event,
        args.at,
        context.clock.now().timestamp(),
        context.policy,
    );

    // `--dry-run` is a read-only preview (like `apply --dry-run`, Inv-18): no at-most-once ledger
    // write, no notification, no status persistence, no fire-log entry — just report the decision.
    if args.dry_run {
        return preview_fire(out, args, &plan.blocks[index], &decision);
    }

    let key = FiredEventKey {
        date: args.date.clone(),
        block_id: args.id.clone(),
        event: args.event,
        rev: args.rev.clone(),
        scheduled_at: args.at,
        attempt: args.attempt,
        agent: None,
    };
    if context.store.check_and_set_fired(key)? == FiredStatus::AlreadyFired {
        return Ok(());
    }

    // The coarse outcome category comes straight from the decision arm; `detail` carries the
    // human-readable specifics each handler appends (e.g. `run-refused: ...`, `notify-failed=...`).
    let outcome = match &decision {
        FireDecision::NoOp => "no-op",
        FireDecision::Notify => "notify",
        FireDecision::Activate { .. } => "activate",
        FireDecision::MarkMissed => "missed",
        FireDecision::Close { .. } => "close",
    };

    let mut detail = String::new();
    let mut run_result = RunExecutionOutcome::NoRun;
    let close_status = if let FireDecision::Close { status } = &decision {
        Some(*status)
    } else {
        None
    };
    let result = match decision {
        FireDecision::NoOp => {
            if args.event == Event::Start && awaiting_approval(&plan.blocks[index]) {
                detail.push_str("awaiting-approval");
            } else {
                detail.push_str("no-op");
            }
            Ok(())
        }
        FireDecision::Notify => {
            log_notify(context, &plan.blocks[index], &mut detail);
            Ok(())
        }
        FireDecision::Activate { run } => {
            activate_block(context, &mut plan, index, run, &mut detail, &mut run_result)
        }
        FireDecision::MarkMissed => mark_missed(context.store, &mut plan, index, &mut detail),
        FireDecision::Close { status } => {
            close_block(context.store, &mut plan, index, status, &mut detail)
        }
    };

    let record = FireRecord {
        ts: context.clock.now().timestamp(),
        date: args.date.clone(),
        id: args.id.clone(),
        event: args.event,
        outcome: outcome.to_owned(),
        detail: detail.trim().to_owned(),
        agent: None,
    };
    append_fire_record(context.store, &record)?;

    // Arm retry or successors after recording the outcome.  Errors here are non-fatal to the
    // primary fire path: the ledger entry is already written.
    let _ = schedule_successors_and_retry(
        context,
        out,
        &plan,
        index,
        outcome,
        close_status,
        run_result,
        args,
    );

    result
}

/// Reports the decision a real `fire` would take, without any side effect.
///
/// Mirrors the human preview of `apply --dry-run`: the scheduler-invoked fire path is the one place
/// that runs `run:` automation, marks blocks, and notifies, so a dry run must touch nothing.
fn preview_fire(
    out: &mut dyn Write,
    args: &FireArgs,
    block: &Block,
    decision: &FireDecision,
) -> Result<()> {
    let action = match decision {
        // The one case worth spelling out: the argv a real fire would execute.
        FireDecision::Activate { run: true } if block.run.is_some() => {
            let command = block.run.as_ref().map_or(&[][..], Run::as_slice);
            format!("would run command: {command:?}")
        }
        other => format!("{other:?}"),
    };
    let line = format!(
        "dry-run: {} {} {} -> {action}",
        args.date, args.id, args.event
    );
    writeln!(out, "{line}")?;
    Ok(())
}

struct FireCleanup;

impl Drop for FireCleanup {
    fn drop(&mut self) {
        crate::platform::cleanup_after_fire();
    }
}

fn status(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let triggers = context.store.list_triggers()?;
    writeln!(out, "triggers: {}", triggers.len())?;
    match context.scheduler.list() {
        Ok(live) => writeln!(out, "live triggers: {}", live.len())?,
        Err(error) => writeln!(out, "live triggers: unavailable ({error})")?,
    }

    // Dead-man check: surface any block whose `expect_by` window has elapsed without a success.
    let now = context.clock.now().timestamp();
    let date = today(context);
    if let Ok(Some(plan)) = context
        .store
        .load_plan_with_default(&date, context.config.notify.default_lead)
    {
        let mut dead = 0u32;
        for block in &plan.blocks {
            let Some(expect_by) = &block.expect_by else {
                continue;
            };
            if context
                .store
                .dead_man_check(&block.id, u64::from(expect_by.as_seconds()), &now)?
            {
                writeln!(
                    out,
                    "dead-man: block `{}` has not succeeded within {}",
                    block.id, expect_by
                )?;
                dead += 1;
            }
        }
        if dead == 0 {
            writeln!(out, "dead-man: ok")?;
        }
    }
    Ok(())
}

fn doctor(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    crate::platform::write_doctor(out, context)?;
    Ok(())
}

fn completions(shell: Shell, out: &mut dyn Write) {
    let mut command = Cli::command();
    match shell {
        Shell::Bash => clap_complete::generate(Bash, &mut command, "ccplan", out),
        Shell::Zsh => clap_complete::generate(Zsh, &mut command, "ccplan", out),
        Shell::Fish => clap_complete::generate(Fish, &mut command, "ccplan", out),
        Shell::Powershell => clap_complete::generate(PowerShell, &mut command, "ccplan", out),
    }
}

fn read_plan_input(source: &str) -> Result<String> {
    if source == "-" {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        Ok(input)
    } else {
        Ok(fs::read_to_string(source)?)
    }
}

fn empty_plan(date: PlanDate, timezone: TimeZoneName) -> Plan {
    Plan {
        date,
        timezone,
        blocks: Vec::new(),
    }
}

/// Runs a mutating command as one transactional read-modify-write under the store lock (Inv-17).
///
/// Threads the configured default notify lead and discards the merged plan the store returns; the
/// caller's `mutate` closure produces the new plan from the loaded one (or `None` when absent).
fn update_plan<F>(context: &ContextRefs<'_>, date: &PlanDate, mutate: F) -> Result<()>
where
    F: FnOnce(Option<Plan>) -> Result<Plan>,
{
    context
        .store
        .update(date, context.config.notify.default_lead, mutate)
        .map(|_| ())
}

/// Unwraps the plan a mutation loaded, turning "no plan for this date" into a `NotFound` error.
fn required_plan(existing: Option<Plan>, date: &PlanDate) -> Result<Plan> {
    existing.ok_or_else(|| Error::NotFound(format!("plan for {date}")))
}

fn load_required(store: &Store, date: &PlanDate, default_lead: Lead) -> Result<Plan> {
    store
        .load_plan_with_default(date, default_lead)?
        .ok_or_else(|| Error::NotFound(format!("plan for {date}")))
}

/// Persists `plan` under the preserve-history policy with the configured default notify lead.
///
/// Centralizes the default-lead wiring and keeps the long `set_plan_with_default` call out of the
/// callers (where rustfmt would wrap it so the `?` lands alone on a line and reads as an uncovered
/// error branch).
fn persist_plan(context: &ContextRefs<'_>, plan: &Plan) -> Result<()> {
    context
        .store
        .set_plan_with_default(
            plan,
            HistoryPolicy::Preserve,
            context.config.notify.default_lead,
        )
        .map(|_| ())
        .map_err(Error::from)
}

/// Applies overdue→`missed`/`expired` reconciliation transitions to `plan` in memory.
///
/// Returns whether anything changed. This is the shared, side-effect-free core: it never touches
/// the store, so callers decide whether to persist (Inv-18: reads don't; `apply`/mutations do).
fn apply_overdue_in_memory(context: &ContextRefs<'_>, plan: &mut Plan) -> Result<bool> {
    let now = context.clock.now().timestamp();
    let updates = reconcile_overdue(plan, now, context.policy.grace())?;
    if updates.is_empty() {
        return Ok(false);
    }
    let by_id = updates
        .into_iter()
        .map(|update| (update.id, update.status))
        .collect::<HashMap<_, _>>();
    for block in &mut plan.blocks {
        if let Some(status) = by_id.get(&block.id) {
            block.status = *status;
        }
    }
    Ok(true)
}

/// Loads a plan and reconciles overdue blocks **in memory only** (Inv-18).
///
/// Used by the read commands (`now`/`next`/`agenda`): a query must never write, must never take the
/// store's write lock, and must leave the plan file byte-identical — so it can't fail with "store
/// locked" against a concurrent writer, and reading never mutates history.
fn read_reconciled_plan(context: &ContextRefs<'_>, date: &PlanDate) -> Result<Plan> {
    let mut plan = load_required(context.store, date, context.config.notify.default_lead)?;
    apply_overdue_in_memory(context, &mut plan)?;
    Ok(plan)
}

/// Loads, reconciles overdue blocks, and **persists** the result transactionally.
///
/// Used by `apply`, a legitimate mutation point. Runs under the store lock (Inv-17) so reconciling
/// overdue blocks can't clobber a block a concurrent writer added to the same day.
fn reconciled_plan(context: &ContextRefs<'_>, date: &PlanDate) -> Result<Plan> {
    context
        .store
        .update(date, context.config.notify.default_lead, |existing| {
            let mut plan = required_plan(existing, date)?;
            apply_overdue_in_memory(context, &mut plan)?;
            Ok(plan)
        })
}

fn desired_triggers(plan: &Plan, now: Timestamp) -> Result<Vec<TriggerRecord>> {
    let mut triggers = Vec::new();
    for block in &plan.blocks {
        if block.status.is_terminal() {
            continue;
        }
        let rev = block.schedule_rev();
        let start = resolve_block_start(plan, block)?;
        let lead = SignedDuration::from_secs(i64::from(block.notify.as_seconds()));
        let notify_at = start
            .checked_sub(lead)
            .map_err(crate::time::TimeError::from)?;
        let end = resolve_block_end(plan, block)?;
        let mut events = vec![(Event::Start, start), (Event::End, end)];
        if notify_at < start {
            events.push((Event::Notify, notify_at));
        }
        for (event, scheduled_at) in events {
            if scheduled_at > now {
                triggers.push(TriggerRecord {
                    backend_id: backend_id_for(&plan.date, &block.id, event, &rev, scheduled_at),
                    date: plan.date.clone(),
                    block_id: block.id.clone(),
                    event,
                    rev: rev.clone(),
                    scheduled_at,
                    kind: TriggerKind::Fire,
                    attempt: 0,
                });
            }
        }
    }
    triggers.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    Ok(triggers)
}

fn reconcile_triggers(
    store: &Store,
    scheduler: &dyn Scheduler,
    date: &PlanDate,
    desired: &[TriggerRecord],
    dry_run: bool,
) -> Result<Vec<ReconcileChange>> {
    let current = store
        .list_triggers()?
        .into_iter()
        .filter(|trigger| &trigger.date == date)
        .collect::<Vec<_>>();
    let desired_by_id = desired
        .iter()
        .map(|trigger| (trigger.backend_id.clone(), trigger))
        .collect::<BTreeMap<_, _>>();
    let current_by_id = current
        .iter()
        .map(|trigger| (trigger.backend_id.clone(), trigger))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::new();

    for trigger in &current {
        if !desired_by_id.contains_key(&trigger.backend_id) {
            changes.push(ReconcileChange::Remove(trigger.backend_id.clone()));
            if !dry_run {
                scheduler.remove(&trigger.backend_id)?;
                store.remove_trigger(&trigger.backend_id)?;
            }
        }
    }
    for trigger in desired {
        if current_by_id.get(&trigger.backend_id).copied() != Some(trigger) {
            changes.push(ReconcileChange::Add(trigger.backend_id.clone()));
            if !dry_run {
                scheduler.add(trigger)?;
                store.record_trigger(trigger.clone())?;
            }
        }
    }

    Ok(changes)
}

fn write_reconcile_summary(out: &mut dyn Write, changes: &[ReconcileChange]) -> Result<()> {
    if changes.is_empty() {
        writeln!(out, "no changes")?;
        return Ok(());
    }
    for change in changes {
        match change {
            ReconcileChange::Add(id) => writeln!(out, "add {id}")?,
            ReconcileChange::Remove(id) => writeln!(out, "remove {id}")?,
        }
    }
    Ok(())
}

/// Writes a list of read-command results: machine `--json`, or a scannable human table.
///
/// The human path renders an aligned, headed table (DESIGN "don't make me think" UX) so `now`/
/// `next`/`agenda` are usable without `--json`; an empty result prints a plain-language line rather
/// than `[]`.
fn write_read_rows<T>(
    out: &mut dyn Write,
    json: bool,
    values: &[T],
    empty_message: &str,
) -> Result<()>
where
    T: Serialize + HumanRow,
{
    if json {
        serde_json::to_writer_pretty(&mut *out, values)?;
        writeln!(out)?;
        return Ok(());
    }
    if values.is_empty() {
        writeln!(out, "{empty_message}")?;
        return Ok(());
    }
    let rows = values.iter().map(HumanRow::columns).collect::<Vec<_>>();
    write_table(out, T::header(), &rows)
}

/// A row renderable as a human table: a static header and one cell string per column.
trait HumanRow {
    fn header() -> &'static [&'static str];
    fn columns(&self) -> Vec<String>;
}

/// Renders `rows` under `header` as a left-aligned, column-padded table. Pure (tested).
fn write_table(out: &mut dyn Write, header: &[&str], rows: &[Vec<String>]) -> Result<()> {
    let mut widths = header
        .iter()
        .map(|cell| cell.chars().count())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let header_cells = header
        .iter()
        .map(|cell| (*cell).to_owned())
        .collect::<Vec<_>>();
    writeln!(out, "{}", format_table_row(&header_cells, &widths))?;
    for row in rows {
        writeln!(out, "{}", format_table_row(row, &widths))?;
    }
    Ok(())
}

/// Pads every cell but the last to its column width (two-space gutter); trims trailing space.
fn format_table_row(cells: &[String], widths: &[usize]) -> String {
    let last = cells.len().saturating_sub(1);
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        if index == last {
            line.push_str(cell);
        } else {
            let _ = write!(line, "{cell:<width$}  ", width = widths[index]);
        }
    }
    line.truncate(line.trim_end().len());
    line
}

/// Human label for a block status (lowercase, stable across the human read commands).
fn status_label(status: Status) -> &'static str {
    status.as_str()
}

/// Renders a non-negative seconds-until-start countdown as a compact human string. Pure (tested).
fn humanize_countdown(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_owned();
    }
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if hours > 0 {
        format!("in {hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("in {minutes}m")
    } else {
        format!("in {seconds}s")
    }
}

fn send_notification(context: &ContextRefs<'_>, block: &Block) -> std::result::Result<(), String> {
    let notification = notification_for(block);
    context
        .notifier
        .notify(&notification)
        .map_err(|error| error.to_string())
}

fn log_notification_result(
    result: std::result::Result<(), String>,
    success_label: &str,
    log_line: &mut String,
) {
    match result {
        Ok(()) => log_line.push_str(success_label),
        Err(error) => {
            log_line.push_str(" notify-failed=");
            log_line.push_str(&sanitize_log_field(&error));
        }
    }
}

fn sanitize_log_field(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_whitespace() { '_' } else { ch })
        .collect()
}

fn log_notify(context: &ContextRefs<'_>, block: &Block, log_line: &mut String) {
    log_notification_result(send_notification(context, block), " notified", log_line);
}

fn notification_for(block: &Block) -> Notification {
    Notification {
        // The title already carries the human-readable block name, so the body need not repeat the
        // slug `id` (which rendered as e.g. "future-focus at 11:00" — redundant and machine-looking).
        title: block.title.clone(),
        body: format!("at {}", block.start),
    }
}

#[cfg(unix)]
#[cfg_attr(coverage_nightly, coverage(off))]
fn check_plan_file_security(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    let mode = metadata.mode();
    if mode & 0o022 != 0 {
        return Err(Error::AutomationRefused(format!(
            "plan file is group- or world-writable (mode: {mode:o})"
        )));
    }
    // Resolve the current UID via a safe syscall wrapper rather than spawning `id` (which is
    // PATH-resolved and brittle in a scheduler-invoked process). `rustix::process::getuid` is
    // infallible and needs no `unsafe`, preserving the crate-wide `#![forbid(unsafe_code)]`.
    let uid = rustix::process::getuid().as_raw();
    if metadata.uid() != uid {
        return Err(Error::AutomationRefused(format!(
            "plan file is not owned by the current user (file uid: {}, current uid: {})",
            metadata.uid(),
            uid
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
// Non-Unix platforms have no equivalent of Unix file permission/ownership checks.
// The caller's return type is Result<()> on all platforms so call sites are uniform.
#[allow(clippy::unnecessary_wraps)]
fn check_plan_file_security(_path: &Path) -> Result<()> {
    Ok(())
}

/// Decides whether a block's `run:` argv is permitted by automation policy.
///
/// Pure (no IO) so the whole policy matrix is unit-testable; the `Err` string is the user-facing
/// refusal reason surfaced as exit code 5 (`Error::AutomationRefused`), per DESIGN §9.
fn authorize_run(
    automation: &AutomationConfig,
    argv: &[String],
) -> std::result::Result<(), String> {
    if !automation.enabled {
        return Err("automation is disabled".to_owned());
    }
    let Some(program) = argv.first() else {
        return Err("empty run command argv".to_owned());
    };
    if !Path::new(program).is_absolute() {
        return Err(format!("executable path is not absolute: {program}"));
    }
    if !automation
        .allowed_executables
        .contains(&std::path::PathBuf::from(program))
    {
        return Err(format!("executable not in allowlist: {program}"));
    }
    Ok(())
}

/// Marks a block active (notifying), runs its `run:` automation if present, then persists.
///
/// Automation is validated and run against the plan file *as loaded* before the activation write,
/// so our own write can't change the perms/ownership the security probe inspects. A refused/failed
/// run is still persisted as `active` (DESIGN §11) and surfaced as the exit code. Policy decisions
/// are pure (`authorize_run`); only the file-security probe and process spawn are IO.
fn activate_block(
    context: &ContextRefs<'_>,
    plan: &mut Plan,
    index: usize,
    run: bool,
    log_line: &mut String,
    run_result: &mut RunExecutionOutcome,
) -> Result<()> {
    plan.blocks[index].status = Status::Active;
    // Per DESIGN §6.3 the `start` event always notifies (the separate heads-up `notify` trigger,
    // when scheduled, fires at an earlier, distinct instant — so this is not a double-notify).
    log_notification_result(
        send_notification(context, &plan.blocks[index]),
        "",
        log_line,
    );

    // Validate + run automation against the plan file *as loaded*, before persisting our own
    // activation write (which would otherwise replace the file's perms/ownership under the
    // security probe). A refusal is still persisted as `active` (DESIGN §11: the block follows
    // its lifecycle regardless of run outcome), then surfaced as the exit code.
    let mut deferred: Option<Error> = None;
    *run_result = RunExecutionOutcome::NoRun;
    match if run {
        plan.blocks[index].run.clone()
    } else {
        None
    } {
        None => log_line.push_str(" activated"),
        Some(run_obj) => {
            let argv = run_obj.as_slice();
            if let Err(reason) = authorize_run(&context.config.automation, argv) {
                let _ = write!(log_line, " run-refused: {reason}");
                *run_result = RunExecutionOutcome::Refused;
                deferred = Some(Error::AutomationRefused(reason));
            } else if let Err(error) =
                check_plan_file_security(&context.store.plan_path(&plan.date))
            {
                let _ = write!(log_line, " run-refused: {error}");
                *run_result = RunExecutionOutcome::Refused;
                deferred = Some(error);
            } else {
                let report = execute_run(argv, context.config.automation.timeout);
                let _ = write!(
                    log_line,
                    " activated run: argv={:?} outcome={} stdout={:?} stderr={:?} rev={} at={}",
                    argv,
                    report.outcome,
                    report.stdout,
                    report.stderr,
                    plan.blocks[index].schedule_rev(),
                    context.clock.now().timestamp()
                );
                *run_result = if report.success {
                    RunExecutionOutcome::Succeeded
                } else {
                    RunExecutionOutcome::Failed
                };
            }
        }
    }

    persist_plan(context, plan)?;

    match deferred {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Whether the `run:` command executed and what it returned.  Pure (tested via `activate_block`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunExecutionOutcome {
    NoRun,
    Succeeded,
    Failed,
    Refused,
}

/// Outcome of a finished (or timed-out) `run:` command, with capped stdout/stderr tails.
struct RunReport {
    outcome: String,
    stdout: String,
    stderr: String,
    success: bool,
}

/// Keeps only the most recent `cap` bytes of a tail buffer. Pure, so the truncation rule is tested.
fn cap_tail(buf: &mut Vec<u8>, cap: usize) {
    if buf.len() > cap {
        let drain = buf.len() - cap;
        buf.drain(0..drain);
    }
}

/// Renders a captured output tail as a single safe log token. Pure (tested).
fn tail_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .replace('\n', "\\n")
        .replace('\r', "")
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn drain_into(reader: &mut impl Read, buf: &std::sync::Mutex<Vec<u8>>, cap: usize) {
    let mut chunk = [0u8; 1024];
    while let Ok(read) = reader.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let mut guard = buf.lock().expect("tail buffer lock is not poisoned");
        guard.extend_from_slice(&chunk[..read]);
        cap_tail(&mut guard, cap);
    }
}

/// Runs an allow-listed argv with no shell, capturing capped output tails and enforcing a timeout.
///
/// This is the genuine process-IO boundary (spawn, reader threads, kill/reap) — excluded from
/// coverage; its pure helpers (`authorize_run`, `cap_tail`, `tail_string`) are tested separately.
#[cfg_attr(coverage_nightly, coverage(off))]
fn execute_run(argv: &[String], timeout: DurationSpec) -> RunReport {
    use std::sync::{Arc, Mutex};
    const TAIL_CAP: usize = 4096;

    let mut child = match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return RunReport {
                outcome: format!("failed-to-spawn:{error}"),
                stdout: String::new(),
                stderr: String::new(),
                success: false,
            };
        }
    };

    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let mut child_stdout = child.stdout.take().expect("stdout was piped");
    let mut child_stderr = child.stderr.take().expect("stderr was piped");
    let stdout_reader = {
        let buf = Arc::clone(&stdout_buf);
        std::thread::spawn(move || drain_into(&mut child_stdout, &buf, TAIL_CAP))
    };
    let stderr_reader = {
        let buf = Arc::clone(&stderr_buf);
        std::thread::spawn(move || drain_into(&mut child_stderr, &buf, TAIL_CAP))
    };

    let deadline = std::time::Duration::from_secs(u64::from(timeout.as_seconds()));
    let start = std::time::Instant::now();
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break "success".to_owned(),
            Ok(Some(status)) => match status.code() {
                Some(code) => break format!("failed:exit={code}"),
                None => break "failed:signal".to_owned(),
            },
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break "timeout".to_owned();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => {
                let _ = child.wait();
                break format!("error:{error}");
            }
        }
    };

    // Join the readers so the captured tails are complete before we read them.
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let success = outcome == "success";
    RunReport {
        outcome,
        stdout: tail_string(
            &stdout_buf
                .lock()
                .expect("stdout buffer lock is not poisoned"),
        ),
        stderr: tail_string(
            &stderr_buf
                .lock()
                .expect("stderr buffer lock is not poisoned"),
        ),
        success,
    }
}

fn mark_block(
    store: &Store,
    plan: &mut Plan,
    index: usize,
    status: Status,
    label: &str,
    log_line: &mut String,
) -> Result<()> {
    plan.blocks[index].status = status;
    log_line.push_str(label);
    store.set_plan(plan, HistoryPolicy::Preserve)?;
    Ok(())
}

fn mark_missed(store: &Store, plan: &mut Plan, index: usize, log_line: &mut String) -> Result<()> {
    mark_block(store, plan, index, Status::Missed, " missed", log_line)
}

fn close_block(
    store: &Store,
    plan: &mut Plan,
    index: usize,
    status: Status,
    log_line: &mut String,
) -> Result<()> {
    mark_block(store, plan, index, status, " closed", log_line)
}

fn append_fire_record(store: &Store, record: &FireRecord) -> Result<()> {
    let path = store.fire_log_path();
    ensure_parent(&path)?;
    let line = serde_json::to_string(record)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

/// Post-fire: arm retries and/or conditional successors through the same apply/scheduler path.
///
/// This is the one new invariant from M5: "only `fire` (successors/retries) and `roll` (horizon)
/// mutate the schedule, both via `apply`."  Errors are non-fatal — the fire ledger entry is
/// already written before this is called.
#[allow(clippy::too_many_arguments)]
fn schedule_successors_and_retry(
    context: &ContextRefs<'_>,
    out: &mut dyn Write,
    plan: &Plan,
    index: usize,
    outcome: &str,
    close_status: Option<Status>,
    run_result: RunExecutionOutcome,
    args: &FireArgs,
) -> Result<()> {
    let block = &plan.blocks[index];
    let now = context.clock.now().timestamp();

    // --- Retry on run failure ---
    if run_result == RunExecutionOutcome::Failed
        && let Some(retry) = &block.retry
        && args.attempt < retry.count
    {
        return arm_retry(
            context,
            plan,
            index,
            args.attempt + 1,
            i64::from(retry.backoff.as_seconds()),
            now,
        );
        // Retry armed; don't arm on_failure successors yet.
    }

    // --- Conditional successors ---
    let successor_ids: Vec<BlockId> = match outcome {
        "close" => {
            if close_status == Some(Status::Done) {
                block.on_success.clone()
            } else {
                block.on_failure.clone()
            }
        }
        "missed" => block.on_missed.clone(),
        "activate"
            if matches!(
                run_result,
                RunExecutionOutcome::Failed | RunExecutionOutcome::Refused
            ) =>
        {
            block.on_failure.clone()
        }
        _ => Vec::new(),
    };

    for id in &successor_ids {
        arm_successor(context, out, plan, id, now)?;
    }

    // --- `after` dependency gate ---
    // When a block closes Done, re-check every other block whose `after` list is now fully done.
    if outcome == "close" && close_status == Some(Status::Done) {
        arm_after_dependents(context, out, plan, &block.id, now)?;
    }

    Ok(())
}

/// Schedules a retry of the block at `now + backoff_secs` with `attempt = next_attempt`.
///
/// Updates the block's start time in the plan so the new `schedule_rev()` matches the retry
/// trigger, then records the trigger in the store and arms it in the OS scheduler.
fn arm_retry(
    context: &ContextRefs<'_>,
    plan: &Plan,
    index: usize,
    next_attempt: u32,
    backoff_secs: i64,
    now: jiff::Timestamp,
) -> Result<()> {
    let retry_ts = now
        .checked_add(jiff::SignedDuration::from_secs(backoff_secs))
        .map_err(crate::time::TimeError::from)?;

    let tz = plan.timezone.to_time_zone().map_err(Error::from)?;
    let retry_zoned = retry_ts.to_zoned(tz);
    let retry_date = PlanDate::from_jiff_date(retry_zoned.date());

    if retry_date != plan.date {
        return Err(Error::Usage(format!(
            "retry for `{}` would cross midnight; increase backoff or reduce retry count",
            plan.blocks[index].id
        )));
    }

    let retry_minutes = u16::from(retry_zoned.hour().unsigned_abs()) * 60
        + u16::from(retry_zoned.minute().unsigned_abs());
    let retry_clock = ClockTime::from_minutes_since_midnight(retry_minutes).map_err(Error::from)?;

    // Persist the updated start time so the block's schedule_rev matches the new trigger.
    let block_id = plan.blocks[index].id.clone();
    context
        .store
        .update(&plan.date, context.config.notify.default_lead, |existing| {
            let mut p =
                existing.ok_or_else(|| Error::NotFound(format!("plan for {}", plan.date)))?;
            if let Some(b) = p.blocks.iter_mut().find(|b| b.id == block_id) {
                b.start = retry_clock;
            }
            Ok::<Plan, Error>(p)
        })?;

    // Compute the trigger with the new rev (from the updated block start).
    let updated_block = {
        let mut tmp = plan.blocks[index].clone();
        tmp.start = retry_clock;
        tmp
    };
    let rev = updated_block.schedule_rev();
    let backend_id = backend_id_for(&plan.date, &updated_block.id, Event::Start, &rev, retry_ts);
    let trigger = TriggerRecord {
        backend_id,
        date: plan.date.clone(),
        block_id: updated_block.id.clone(),
        event: Event::Start,
        rev,
        scheduled_at: retry_ts,
        kind: TriggerKind::Fire,
        attempt: next_attempt,
    };

    context.scheduler.prepare()?;
    context.store.record_trigger(trigger.clone())?;
    context.scheduler.add(&trigger)?;
    Ok(())
}

/// Arms a successor block by setting its start to `now + grace` and calling `apply`.
///
/// Refuses a successor that would cross midnight (same restriction as snooze).
fn arm_successor(
    context: &ContextRefs<'_>,
    out: &mut dyn Write,
    plan: &Plan,
    successor_id: &BlockId,
    now: jiff::Timestamp,
) -> Result<()> {
    const SUCCESSOR_GRACE_SECS: i64 = 30;

    let successor_ts = now
        .checked_add(jiff::SignedDuration::from_secs(SUCCESSOR_GRACE_SECS))
        .map_err(crate::time::TimeError::from)?;

    let tz = plan.timezone.to_time_zone().map_err(Error::from)?;
    let successor_zoned = successor_ts.to_zoned(tz);
    let successor_date = PlanDate::from_jiff_date(successor_zoned.date());

    if successor_date != plan.date {
        return Err(Error::Usage(format!(
            "successor `{successor_id}` would start past midnight; skipping"
        )));
    }

    let successor_minutes = u16::from(successor_zoned.hour().unsigned_abs()) * 60
        + u16::from(successor_zoned.minute().unsigned_abs());
    let successor_clock =
        ClockTime::from_minutes_since_midnight(successor_minutes).map_err(Error::from)?;

    let sid = successor_id.clone();
    context
        .store
        .update(&plan.date, context.config.notify.default_lead, |existing| {
            let mut p =
                existing.ok_or_else(|| Error::NotFound(format!("plan for {}", plan.date)))?;
            if let Some(b) = p.blocks.iter_mut().find(|b| b.id == sid)
                && !b.status.is_terminal()
            {
                b.start = successor_clock;
                b.status = Status::Pending;
            }
            Ok::<Plan, Error>(p)
        })?;

    apply(
        ApplyArgs {
            date: Some(plan.date.clone()),
            dry_run: false,
        },
        out,
        context,
    )
}

/// Arms any block in the plan whose `after` list is now fully satisfied by Done blocks.
fn arm_after_dependents(
    context: &ContextRefs<'_>,
    out: &mut dyn Write,
    plan: &Plan,
    completed_id: &BlockId,
    now: jiff::Timestamp,
) -> Result<()> {
    let done_ids: std::collections::HashSet<&BlockId> = plan
        .blocks
        .iter()
        .filter(|b| b.status == Status::Done)
        .map(|b| &b.id)
        .collect();

    for block in &plan.blocks {
        if block.after.is_empty() || block.status.is_terminal() {
            continue;
        }
        if !block.after.contains(completed_id) {
            continue;
        }
        if block.after.iter().all(|dep| done_ids.contains(dep)) {
            arm_successor(context, out, plan, &block.id.clone(), now)?;
        }
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    Ok(())
}

fn today(context: &ContextRefs<'_>) -> PlanDate {
    PlanDate::from_jiff_date(context.clock.now().date())
}

fn timezone_from_clock(context: &ContextRefs<'_>) -> Result<TimeZoneName> {
    context
        .clock
        .now()
        .time_zone()
        .iana_name()
        .unwrap_or("Etc/UTC")
        .parse()
        .map_err(Error::from)
}

fn span_from(end: Option<ClockTime>, duration: Option<DurationSpec>) -> Result<Span> {
    match (end, duration) {
        (Some(end), None) => Ok(Span::End(end)),
        (None, Some(duration)) => Ok(Span::Duration(duration)),
        (Some(_), Some(_)) | (None, None) => Err(Error::Usage(
            "set exactly one of --end or --duration".to_owned(),
        )),
    }
}

fn run_from(run: Vec<String>) -> Result<Option<Run>> {
    if run.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Run::new(run)?))
    }
}

fn find_block_mut<'a>(plan: &'a mut Plan, id: &BlockId) -> Result<&'a mut Block> {
    plan.blocks
        .iter_mut()
        .find(|block| &block.id == id)
        .ok_or_else(|| Error::NotFound(format!("block `{id}`")))
}

fn ensure_non_terminal(block: &Block) -> Result<()> {
    if block.status.is_terminal() {
        Err(Error::HistoryConflict {
            id: block.id.clone(),
        })
    } else {
        Ok(())
    }
}

pub(crate) fn slug_block_id(title: &str) -> Result<BlockId> {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("block");
    }
    BlockId::new(slug).map_err(Error::from)
}

fn backend_id_for(
    date: &PlanDate,
    id: &BlockId,
    event: Event,
    rev: &crate::model::ScheduleRev,
    _scheduled_at: Timestamp,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(id.as_str().as_bytes());
    let id_hash = hasher.finalize().to_hex()[..10].to_owned();
    format!("{date}-{id_hash}-{rev}-{event}")
}

fn format_end(block: &Block) -> String {
    format_seconds_as_clock(block.span.resolved_end_seconds(block.start))
}

fn format_seconds_as_clock(seconds: u32) -> String {
    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    if seconds == 0 {
        format!("{hours:02}:{minutes:02}")
    } else {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReconcileChange {
    Add(String),
    Remove(String),
}

#[derive(Debug, Serialize)]
struct BlockSummary {
    id: String,
    title: String,
    start: String,
    end: String,
    status: Status,
}

impl BlockSummary {
    fn from_block(block: &Block) -> Self {
        Self {
            id: block.id.to_string(),
            title: block.title.clone(),
            start: block.start.to_string(),
            end: format_end(block),
            status: block.status,
        }
    }
}

impl HumanRow for BlockSummary {
    fn header() -> &'static [&'static str] {
        &["TIME", "STATUS", "ID", "TITLE"]
    }

    fn columns(&self) -> Vec<String> {
        vec![
            format!("{}-{}", self.start, self.end),
            status_label(self.status).to_owned(),
            self.id.clone(),
            self.title.clone(),
        ]
    }
}

#[derive(Debug, Serialize)]
struct AgendaEntry {
    #[serde(flatten)]
    block: BlockSummary,
    starts_in_seconds: i64,
}

impl AgendaEntry {
    fn new(block: &Block, starts_in_seconds: i64) -> Self {
        Self {
            block: BlockSummary::from_block(block),
            starts_in_seconds,
        }
    }
}

impl HumanRow for AgendaEntry {
    fn header() -> &'static [&'static str] {
        &["TIME", "IN", "STATUS", "ID", "TITLE"]
    }

    fn columns(&self) -> Vec<String> {
        vec![
            format!("{}-{}", self.block.start, self.block.end),
            humanize_countdown(self.starts_in_seconds),
            status_label(self.block.status).to_owned(),
            self.block.id.clone(),
            self.block.title.clone(),
        ]
    }
}

#[cfg(test)]
mod condition_probe_tests {
    use super::{ConditionProbe, RealConditionProbe, file_changed_marker, file_changed_state};
    use crate::{
        config::AutomationConfig, error::Error, model::WhenCondition, serve::ConditionState,
    };
    use std::{
        io::{Error as IoError, ErrorKind},
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    #[test]
    fn file_changed_marker_uses_epoch_seconds_and_nanos() {
        let modified = UNIX_EPOCH + Duration::new(12, 34);
        assert_eq!(file_changed_marker(modified), "mtime:12:34");
    }

    #[test]
    fn file_changed_marker_defaults_for_pre_epoch_times() {
        let modified = UNIX_EPOCH - Duration::new(1, 0);
        assert_eq!(file_changed_marker(modified), "mtime:0:0");
    }

    #[test]
    fn file_changed_state_maps_filesystem_results() {
        assert_eq!(
            file_changed_state(Ok(UNIX_EPOCH + Duration::new(12, 34))).unwrap(),
            ConditionState::satisfied("mtime:12:34")
        );
        assert_eq!(
            file_changed_state(Err(IoError::new(ErrorKind::NotFound, "missing"))).unwrap(),
            ConditionState::unsatisfied()
        );
        assert!(file_changed_state(Err(IoError::new(ErrorKind::PermissionDenied, "no"))).is_err());
    }

    #[test]
    fn real_condition_probe_reports_file_exists_states() {
        let temp = assert_fs::TempDir::new().unwrap();
        let ready = temp.path().join("ready.flag");
        std::fs::write(&ready, "ready").unwrap();
        let missing = temp.path().join("missing.flag");
        let automation = AutomationConfig::default();
        let probe = RealConditionProbe {
            automation: &automation,
        };

        assert_eq!(
            probe
                .state(&WhenCondition::FileExists(
                    ready.to_string_lossy().into_owned()
                ))
                .unwrap(),
            ConditionState::satisfied("exists")
        );
        assert_eq!(
            probe
                .state(&WhenCondition::FileExists(
                    missing.to_string_lossy().into_owned()
                ))
                .unwrap(),
            ConditionState::unsatisfied()
        );
    }

    #[test]
    fn real_condition_probe_reports_file_changed_states() {
        let temp = assert_fs::TempDir::new().unwrap();
        let input = temp.path().join("input.txt");
        std::fs::write(&input, "changed").unwrap();
        let missing = temp.path().join("missing.txt");
        let automation = AutomationConfig::default();
        let probe = RealConditionProbe {
            automation: &automation,
        };

        assert!(
            probe
                .state(&WhenCondition::FileChanged(
                    input.to_string_lossy().into_owned()
                ))
                .unwrap()
                .satisfied
        );
        assert_eq!(
            probe
                .state(&WhenCondition::FileChanged(
                    missing.to_string_lossy().into_owned()
                ))
                .unwrap(),
            ConditionState::unsatisfied()
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_condition_probe_reports_command_ok_success_and_failure() {
        let automation = AutomationConfig {
            enabled: true,
            allowed_executables: vec![PathBuf::from("/bin/true"), PathBuf::from("/bin/false")],
            ..AutomationConfig::default()
        };
        let probe = RealConditionProbe {
            automation: &automation,
        };

        assert_eq!(
            probe
                .state(&WhenCondition::CommandOk(vec!["/bin/true".to_owned()]))
                .unwrap(),
            ConditionState::satisfied("ok")
        );
        assert_eq!(
            probe
                .state(&WhenCondition::CommandOk(vec!["/bin/false".to_owned()]))
                .unwrap(),
            ConditionState::unsatisfied()
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_condition_probe_maps_command_ok_policy_refusal() {
        let automation = AutomationConfig::default();
        let probe = RealConditionProbe {
            automation: &automation,
        };

        let error = probe
            .state(&WhenCondition::CommandOk(vec!["/bin/true".to_owned()]))
            .unwrap_err();
        assert!(matches!(error, Error::AutomationRefused(_)));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod read_output_tests {
    use super::{
        HumanRow, Status, format_table_row, humanize_countdown, status_label, write_table,
    };

    #[test]
    fn humanize_countdown_covers_each_unit_branch() {
        assert_eq!(humanize_countdown(0), "now");
        assert_eq!(humanize_countdown(-5), "now");
        assert_eq!(humanize_countdown(45), "in 45s");
        assert_eq!(humanize_countdown(120), "in 2m");
        assert_eq!(humanize_countdown(3_600), "in 1h00m");
        assert_eq!(humanize_countdown(3_600 + 5 * 60), "in 1h05m");
    }

    #[test]
    fn status_label_covers_every_status() {
        assert_eq!(status_label(Status::Pending), "pending");
        assert_eq!(status_label(Status::Active), "active");
        assert_eq!(status_label(Status::Done), "done");
        assert_eq!(status_label(Status::Skipped), "skipped");
        assert_eq!(status_label(Status::Missed), "missed");
        assert_eq!(status_label(Status::Expired), "expired");
    }

    #[test]
    fn table_aligns_columns_and_trims_trailing_space() {
        // A short cell is padded to its column width; the final column is never padded, so no row
        // carries trailing whitespace.
        let widths = [5, 3];
        assert_eq!(
            format_table_row(&["ab".to_owned(), "x".to_owned()], &widths),
            "ab     x"
        );
        assert_eq!(
            format_table_row(&["abcde".to_owned(), String::new()], &widths),
            "abcde"
        );

        let mut out = Vec::new();
        write_table(
            &mut out,
            &["ID", "TITLE"],
            &[vec!["a".to_owned(), "Alpha".to_owned()]],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "ID  TITLE\na   Alpha\n");
    }

    #[test]
    fn agenda_entry_row_includes_countdown_column() {
        let header = <super::AgendaEntry as HumanRow>::header();
        assert_eq!(header, &["TIME", "IN", "STATUS", "ID", "TITLE"]);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod watch_tests {
    use super::{WATCH_CLEAR, WatchClock, WatchSignal, render_watch_frame, watch_loop};
    use crate::{
        config::Config,
        context::{Context, RecordingNotifier, RecordingScheduler},
        model::Plan,
        store::{HistoryPolicy, Store},
        time::FixedClock,
    };
    use assert_fs::TempDir;
    use jiff::Zoned;

    /// A scripted refresh driver: replays a fixed signal sequence, then quits. No timer, no input.
    struct ScriptedClock {
        signals: std::vec::IntoIter<WatchSignal>,
    }

    impl WatchClock for ScriptedClock {
        fn wait(&mut self) -> WatchSignal {
            self.signals.next().unwrap_or(WatchSignal::Quit)
        }
    }

    fn context_at(
        now: &str,
    ) -> (
        TempDir,
        Context<FixedClock, RecordingScheduler, RecordingNotifier>,
    ) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let clock = FixedClock::new(now.parse::<Zoned>().unwrap());
        let context = Context::new(
            store,
            clock,
            RecordingScheduler::default(),
            RecordingNotifier::default(),
            Config::default(),
        );
        let plan = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus"
title = "Focus time"
start = "11:00"
end = "11:30"
status = "pending"
"#,
        )
        .unwrap();
        context
            .store
            .set_plan(&plan, HistoryPolicy::Preserve)
            .unwrap();
        (temp, context)
    }

    #[test]
    fn watch_loop_redraws_each_refresh_and_stops_on_quit() {
        let (_temp, context) = context_at("2026-06-08T10:50:00+05:30[Asia/Kolkata]");
        let refs = context.as_refs();
        let mut out = Vec::new();
        let mut driver = ScriptedClock {
            // Refresh draws a second frame; Quit ends the loop after it.
            signals: vec![WatchSignal::Refresh, WatchSignal::Quit].into_iter(),
        };

        watch_loop(None, &mut out, &refs, &mut driver).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.matches("ccplan watch ·").count(),
            2,
            "two frames: {text}"
        );
        assert!(
            text.contains(WATCH_CLEAR),
            "frames clear the screen: {text}"
        );
    }

    #[test]
    fn render_watch_frame_carries_header_clock_and_live_agenda() {
        let (_temp, context) = context_at("2026-06-08T10:50:00+05:30[Asia/Kolkata]");
        let refs = context.as_refs();

        let frame = render_watch_frame(&refs, &"2026-06-08".parse().unwrap()).unwrap();

        assert!(
            frame.starts_with("ccplan watch · 2026-06-08 · 10:50:00"),
            "{frame}"
        );
        assert!(frame.contains("Ctrl-C or Enter to quit"), "{frame}");
        // The block is active at 11:10, so the live agenda row renders inside the frame.
        assert!(frame.contains("Focus time"), "{frame}");
    }

    #[test]
    fn render_watch_frame_shows_empty_agenda_when_nothing_remains() {
        // After the only block has ended, the frame still draws but the agenda is empty.
        let (_temp, context) = context_at("2026-06-08T23:00:00+05:30[Asia/Kolkata]");
        let refs = context.as_refs();

        let frame = render_watch_frame(&refs, &"2026-06-08".parse().unwrap()).unwrap();

        assert!(frame.contains("nothing left on today's agenda"), "{frame}");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod automation_policy_tests {
    use super::{authorize_run, cap_tail, tail_string};
    use crate::config::AutomationConfig;
    use std::path::PathBuf;

    fn enabled_with(allow: &[&str]) -> AutomationConfig {
        AutomationConfig {
            enabled: true,
            allowed_executables: allow.iter().map(|s| PathBuf::from(*s)).collect(),
            ..AutomationConfig::default()
        }
    }

    // On Windows, a truly absolute path requires a drive letter (e.g. C:\…); Unix-style /bin/echo
    // is root-relative and Path::is_absolute() returns false. Use platform-specific literals.
    #[cfg(unix)]
    const ABS_PATH_A: &str = "/bin/echo";
    #[cfg(unix)]
    const ABS_PATH_B: &str = "/bin/true";
    #[cfg(windows)]
    const ABS_PATH_A: &str = r"C:\Windows\System32\cmd.exe";
    #[cfg(windows)]
    const ABS_PATH_B: &str = r"C:\Windows\System32\notepad.exe";
    // Fallback for non-unix, non-windows (unsupported tier)
    #[cfg(not(any(unix, windows)))]
    const ABS_PATH_A: &str = "/bin/echo";
    #[cfg(not(any(unix, windows)))]
    const ABS_PATH_B: &str = "/bin/true";

    #[test]
    fn authorize_run_allows_absolute_allowlisted_program() {
        let config = enabled_with(&[ABS_PATH_A]);
        assert!(authorize_run(&config, &[ABS_PATH_A.to_owned(), "hi".to_owned()]).is_ok());
    }

    #[test]
    fn authorize_run_refuses_when_disabled() {
        assert_eq!(
            authorize_run(&AutomationConfig::default(), &[ABS_PATH_A.to_owned()]),
            Err("automation is disabled".to_owned())
        );
    }

    #[test]
    fn authorize_run_refuses_empty_argv() {
        assert!(
            authorize_run(&enabled_with(&[]), &[])
                .unwrap_err()
                .contains("empty run command argv")
        );
    }

    #[test]
    fn authorize_run_refuses_relative_program() {
        assert!(
            authorize_run(&enabled_with(&["echo"]), &["echo".to_owned()])
                .unwrap_err()
                .contains("executable path is not absolute")
        );
    }

    #[test]
    fn authorize_run_refuses_unlisted_program() {
        assert!(
            authorize_run(&enabled_with(&[ABS_PATH_B]), &[ABS_PATH_A.to_owned()])
                .unwrap_err()
                .contains("executable not in allowlist")
        );
    }

    #[test]
    fn cap_tail_keeps_only_most_recent_bytes() {
        let mut buf = b"0123456789".to_vec();
        cap_tail(&mut buf, 4);
        assert_eq!(buf, b"6789");
        let mut small = b"ab".to_vec();
        cap_tail(&mut small, 4);
        assert_eq!(small, b"ab");
    }

    #[test]
    fn tail_string_trims_and_escapes_newlines() {
        assert_eq!(tail_string(b"  a\nb\r\n  "), "a\\nb");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod arm_tests {
    use super::{arm_retry, arm_successor};
    use crate::{
        config::Config,
        context::{Context, RecordingNotifier, RecordingScheduler},
        model::{BlockId, Plan},
        store::{HistoryPolicy, Store},
        time::FixedClock,
    };
    use assert_fs::TempDir;
    use jiff::Zoned;

    fn make_ctx(
        now_str: &str,
    ) -> (
        TempDir,
        Context<FixedClock, RecordingScheduler, RecordingNotifier>,
    ) {
        let temp = TempDir::new().unwrap();
        let context = Context::new(
            Store::new(temp.path()),
            FixedClock::new(now_str.parse::<Zoned>().unwrap()),
            RecordingScheduler::default(),
            RecordingNotifier::default(),
            Config::default(),
        );
        (temp, context)
    }

    const PLAN_WITH_FOCUS: &str = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"
[[block]]
id = "focus"
title = "Focus"
start = "10:00"
end = "10:30"
status = "active"
"#;

    const PLAN_WITH_OTHER: &str = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"
[[block]]
id = "other-block"
title = "Other"
start = "11:00"
end = "11:30"
status = "pending"
"#;

    const PLAN_WITH_NEXT_STEP: &str = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"
[[block]]
id = "next-step"
title = "Next Step"
start = "11:00"
end = "11:30"
status = "pending"
"#;

    #[test]
    fn arm_retry_errors_when_plan_absent_from_store() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        let err = arm_retry(&ctx.as_refs(), &plan, 0, 1, 60, now).unwrap_err();
        assert!(err.to_string().contains("plan for"), "{err}");
    }

    #[test]
    fn arm_retry_skips_update_when_block_id_absent_from_stored_plan() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        ctx.store
            .set_plan(
                &Plan::from_toml(PLAN_WITH_OTHER).unwrap(),
                HistoryPolicy::Preserve,
            )
            .unwrap();
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        arm_retry(&ctx.as_refs(), &plan, 0, 1, 60, now).unwrap();
    }

    #[test]
    fn arm_retry_updates_block_start_when_block_exists() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        ctx.store.set_plan(&plan, HistoryPolicy::Preserve).unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        arm_retry(&ctx.as_refs(), &plan, 0, 1, 60, now).unwrap();
    }

    #[test]
    fn arm_successor_errors_when_plan_absent_from_store() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let sid: BlockId = "next-step".parse().unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        let err = arm_successor(&ctx.as_refs(), &mut Vec::new(), &plan, &sid, now).unwrap_err();
        assert!(err.to_string().contains("plan for"), "{err}");
    }

    #[test]
    fn arm_successor_is_noop_when_block_id_absent_from_stored_plan() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        ctx.store
            .set_plan(
                &Plan::from_toml(PLAN_WITH_OTHER).unwrap(),
                HistoryPolicy::Preserve,
            )
            .unwrap();
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let sid: BlockId = "next-step".parse().unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        arm_successor(&ctx.as_refs(), &mut Vec::new(), &plan, &sid, now).unwrap();
    }

    #[test]
    fn arm_successor_updates_pending_successor_when_found() {
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        ctx.store
            .set_plan(
                &Plan::from_toml(PLAN_WITH_NEXT_STEP).unwrap(),
                HistoryPolicy::Preserve,
            )
            .unwrap();
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let sid: BlockId = "next-step".parse().unwrap();
        let now: jiff::Timestamp = "2026-06-08T10:00:00+05:30".parse().unwrap();
        arm_successor(&ctx.as_refs(), &mut Vec::new(), &plan, &sid, now).unwrap();
    }

    #[test]
    fn arm_retry_errors_when_retry_would_cross_midnight() {
        let (_temp, ctx) = make_ctx("2026-06-08T23:59:00+05:30[Asia/Kolkata]");
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        // 120 seconds from 23:59:00 → 2026-06-09T00:01:00, which crosses midnight
        let now: jiff::Timestamp = "2026-06-08T23:59:00+05:30".parse().unwrap();
        let err = arm_retry(&ctx.as_refs(), &plan, 0, 1, 120, now).unwrap_err();
        assert!(err.to_string().contains("cross midnight"), "{err}");
    }

    #[test]
    fn arm_successor_errors_when_successor_would_cross_midnight() {
        let (_temp, ctx) = make_ctx("2026-06-08T23:59:45+05:30[Asia/Kolkata]");
        let plan = Plan::from_toml(PLAN_WITH_FOCUS).unwrap();
        let sid: BlockId = "next-step".parse().unwrap();
        // SUCCESSOR_GRACE_SECS=30 from 23:59:45 → 2026-06-09T00:00:15, which crosses midnight
        let now: jiff::Timestamp = "2026-06-08T23:59:45+05:30".parse().unwrap();
        let err = arm_successor(&ctx.as_refs(), &mut Vec::new(), &plan, &sid, now).unwrap_err();
        assert!(err.to_string().contains("past midnight"), "{err}");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod gui_dispatch_tests {
    use super::dispatch;
    use crate::{
        config::Config,
        context::{Context, RecordingNotifier, RecordingScheduler},
        store::Store,
        time::FixedClock,
    };
    use assert_fs::TempDir;
    use jiff::Zoned;

    fn make_ctx(
        now_str: &str,
    ) -> (
        TempDir,
        Context<FixedClock, RecordingScheduler, RecordingNotifier>,
    ) {
        let temp = TempDir::new().unwrap();
        let context = Context::new(
            Store::new(temp.path()),
            FixedClock::new(now_str.parse::<Zoned>().unwrap()),
            RecordingScheduler::default(),
            RecordingNotifier::default(),
            Config::default(),
        );
        (temp, context)
    }

    #[test]
    fn gui_dispatch_launches_cockpit() {
        // launch_cockpit has a cfg(test) early-return so this just covers the dispatch arm.
        let (_temp, ctx) = make_ctx("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        dispatch(
            Some(crate::cli::Commands::Gui),
            &mut Vec::new(),
            &ctx.as_refs(),
        )
        .unwrap();
    }
}
