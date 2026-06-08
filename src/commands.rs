//! Command dispatch and platform-agnostic command behavior.

use std::{
    collections::{BTreeMap, HashMap},
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
        AddArgs, AgendaArgs, ApplyArgs, ClearArgs, Cli, Commands, EditArgs, FireArgs, ReadArgs,
        SetArgs, Shell,
    },
    config::AutomationConfig,
    context::{ContextRefs, Notification, Scheduler},
    error::{Error, Result},
    lifecycle::{Event, FireDecision, decide_fire, reconcile_overdue},
    model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName,
    },
    store::{FiredEventKey, FiredStatus, HistoryPolicy, Store, TriggerRecord},
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
        Some(Commands::Edit(args)) => edit(args, context),
        Some(Commands::Rm(args)) => remove(&args.id, context),
        Some(Commands::Done(args)) => set_status(args.id, Status::Done, context),
        Some(Commands::Skip(args)) => set_status(args.id, Status::Skipped, context),
        Some(Commands::Clear(args)) => clear(args, out, context),
        Some(Commands::Show(args)) => show(args, out, context),
        Some(Commands::Now(args)) => now(args, out, context),
        Some(Commands::Next(args)) => next(args, out, context),
        Some(Commands::Agenda(args)) => agenda(args, out, context),
        Some(Commands::Apply(args)) => apply(args, out, context),
        Some(Commands::Fire(args)) => fire(&args, out, context),
        Some(Commands::Status) => status(out, context),
        Some(Commands::Doctor) => doctor(out, context),
        Some(Commands::Completions(args)) => {
            completions(args.shell, out);
            Ok(())
        }
    }
}

fn set(args: SetArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let input = read_plan_input(&args.from)?;
    let mut plan = Plan::from_toml_with_default(&input, context.config.notify.default_lead)?;
    if let Some(date) = args.date {
        plan.date = date;
    }
    let policy = if args.override_history {
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

fn add(args: AddArgs, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let id = match args.id {
        Some(id) => id,
        None => slug_block_id(&args.title)?,
    };
    let block = Block {
        id: id.clone(),
        title: args.title,
        start: args.start,
        span: span_from(args.end, args.duration)?,
        notify: args.notify.unwrap_or(context.config.notify.default_lead),
        tags: args.tags,
        status: Status::Pending,
        run: run_from(args.run)?,
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

fn fire(args: &FireArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let _cleanup = FireCleanup;
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

    let key = FiredEventKey {
        date: args.date.clone(),
        block_id: args.id.clone(),
        event: args.event,
        rev: args.rev.clone(),
        scheduled_at: args.at,
    };
    if context.store.check_and_set_fired(key)? == FiredStatus::AlreadyFired {
        return Ok(());
    }

    let decision = decide_fire(
        &plan.blocks[index],
        args.event,
        args.at,
        context.clock.now().timestamp(),
        context.policy,
    );
    let mut log_line = format!("{} {} {}", args.date, args.id, args.event);

    // We run the match and if it errors, we STILL want to write the log line if we constructed one
    // (especially if it was refused, where check_plan_file_security logs the refusal inside activate_block).
    let result = match decision {
        FireDecision::NoOp => {
            log_line.push_str(" no-op");
            Ok(())
        }
        FireDecision::Notify => {
            log_notify(context, &plan.blocks[index], &mut log_line);
            Ok(())
        }
        FireDecision::Activate { run } => activate_block(
            context,
            &mut plan,
            index,
            run,
            args.dry_run,
            out,
            &mut log_line,
        ),
        FireDecision::MarkMissed => mark_missed(context.store, &mut plan, index, &mut log_line),
        FireDecision::Close { status } => {
            close_block(context.store, &mut plan, index, status, &mut log_line)
        }
    };

    append_fire_log(context.store, &log_line)?;
    result
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
    match status {
        Status::Pending => "pending",
        Status::Active => "active",
        Status::Done => "done",
        Status::Skipped => "skipped",
        Status::Missed => "missed",
        Status::Expired => "expired",
    }
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
        title: block.title.clone(),
        body: format!("{} at {}", block.id, block.start),
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
    let output = std::process::Command::new("id").arg("-u").output();
    let uid = match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            s.trim().parse::<u32>().unwrap_or(u32::MAX)
        }
        _ => {
            return Err(Error::AutomationRefused(
                "failed to determine current user UID".to_string(),
            ));
        }
    };
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
    dry_run: bool,
    out: &mut dyn Write,
    log_line: &mut String,
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
                deferred = Some(Error::AutomationRefused(reason));
            } else if let Err(error) =
                check_plan_file_security(&context.store.plan_path(&plan.date))
            {
                let _ = write!(log_line, " run-refused: {error}");
                deferred = Some(error);
            } else if dry_run {
                writeln!(out, "dry-run: would run command: {argv:?}")?;
                let _ = write!(log_line, " activated run (dry-run): argv={argv:?}");
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
            }
        }
    }

    persist_plan(context, plan)?;

    match deferred {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Outcome of a finished (or timed-out) `run:` command, with capped stdout/stderr tails.
struct RunReport {
    outcome: String,
    stdout: String,
    stderr: String,
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

fn append_fire_log(store: &Store, line: &str) -> Result<()> {
    let path = store.fire_log_path();
    ensure_parent(&path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
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

fn slug_block_id(title: &str) -> Result<BlockId> {
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
