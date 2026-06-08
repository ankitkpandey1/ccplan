//! Command dispatch and platform-agnostic command behavior.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::{
    cli::{
        AddArgs, AgendaArgs, ApplyArgs, ClearArgs, Commands, EditArgs, FireArgs, ReadArgs, SetArgs,
        Shell,
    },
    context::{ContextRefs, Notification, Scheduler},
    error::{Error, Result},
    lifecycle::{Event, FireDecision, decide_fire, reconcile_overdue},
    model::{
        Block, BlockId, ClockTime, DurationSpec, Plan, PlanDate, Run, Span, Status, TimeZoneName,
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
        Some(Commands::Fire(args)) => fire(&args, context),
        Some(Commands::Status) => status(out, context),
        Some(Commands::Doctor) => doctor(out, context),
        Some(Commands::Completions(args)) => completions(args.shell, out),
    }
}

fn set(args: SetArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let input = read_plan_input(&args.from)?;
    let mut plan = Plan::from_toml(&input)?;
    if let Some(date) = args.date {
        plan.date = date;
    }
    let policy = if args.override_history {
        HistoryPolicy::Override
    } else {
        HistoryPolicy::Preserve
    };
    let stored = context.store.set_plan(&plan, policy)?;
    writeln!(out, "stored {}", stored.date)?;
    Ok(())
}

fn add(args: AddArgs, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let mut plan = load_or_new_plan(context.store, &date, context)?;
    let id = match args.id {
        Some(id) => id,
        None => slug_block_id(&args.title)?,
    };
    let block = Block {
        id: id.clone(),
        title: args.title,
        start: args.start,
        span: span_from(args.end, args.duration)?,
        notify: args.notify.unwrap_or_default(),
        tags: args.tags,
        status: Status::Pending,
        run: run_from(args.run)?,
    };

    match plan.blocks.iter().position(|existing| existing.id == id) {
        Some(index) if plan.blocks[index].status.is_terminal() => {
            Err(Error::HistoryConflict { id })
        }
        Some(index) => replace_block(context.store, &mut plan, index, block),
        None => insert_block(context.store, &mut plan, block),
    }
}

fn edit(args: EditArgs, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let mut plan = load_required(context.store, &date)?;
    let block = find_block_mut(&mut plan, &args.id)?;
    ensure_non_terminal(block)?;

    if let Some(title) = args.title {
        block.title = title;
    }
    if let Some(start) = args.start {
        block.start = start;
    }
    if args.end.is_some() && args.duration.is_some() {
        return Err(Error::Usage(
            "edit accepts only one of --end or --duration".to_owned(),
        ));
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

    context.store.set_plan(&plan, HistoryPolicy::Preserve)?;
    Ok(())
}

fn remove(id: &BlockId, context: &ContextRefs<'_>) -> Result<()> {
    let date = today(context);
    let mut plan = load_required(context.store, &date)?;
    let index = plan
        .blocks
        .iter()
        .position(|block| &block.id == id)
        .ok_or_else(|| Error::NotFound(format!("block `{id}`")))?;
    ensure_non_terminal(&plan.blocks[index])?;
    plan.blocks.remove(index);
    context.store.set_plan(&plan, HistoryPolicy::Preserve)?;
    Ok(())
}

fn set_status(id: BlockId, status: Status, context: &ContextRefs<'_>) -> Result<()> {
    let date = today(context);
    let mut plan = load_required(context.store, &date)?;
    let block = find_block_mut(&mut plan, &id)?;
    if block.status.is_terminal() && block.status != status {
        return Err(Error::HistoryConflict { id });
    }
    block.status = status;
    context.store.set_plan(&plan, HistoryPolicy::Preserve)?;
    Ok(())
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
    let plan = load_required(context.store, &date)?;
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
    let plan = reconciled_plan(context, &date)?;
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
    write_read_array(out, args.json, &blocks)
}

fn next(args: ReadArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = reconciled_plan(context, &date)?;
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
        return write_read_array(out, args.json, &Vec::<BlockSummary>::new());
    };
    let blocks = candidates
        .into_iter()
        .filter_map(|(start, block)| (start == next_start).then_some(block))
        .collect::<Vec<_>>();
    write_read_array(out, args.json, &blocks)
}

fn agenda(args: AgendaArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = reconciled_plan(context, &date)?;
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
    write_read_array(out, args.json, &blocks)
}

fn apply(args: ApplyArgs, out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    let date = args.date.unwrap_or_else(|| today(context));
    let plan = reconciled_plan(context, &date)?;
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

fn fire(args: &FireArgs, context: &ContextRefs<'_>) -> Result<()> {
    let _cleanup = FireCleanup;
    let Some(mut plan) = context.store.load_plan(&args.date)? else {
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
    match decision {
        FireDecision::NoOp => log_line.push_str(" no-op"),
        FireDecision::Notify => log_notify(context, &plan.blocks[index], &mut log_line),
        FireDecision::Activate { notify, run } => {
            activate_block(context, &mut plan, index, notify, run, &mut log_line)?;
        }
        FireDecision::MarkMissed => mark_missed(context.store, &mut plan, index, &mut log_line)?,
        FireDecision::Close { status } => {
            close_block(context.store, &mut plan, index, status, &mut log_line)?;
        }
    }
    append_fire_log(context.store, &log_line)?;
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
    Ok(())
}

fn doctor(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    crate::platform::write_doctor(out, context)?;
    Ok(())
}

fn completions(shell: Shell, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "# {shell} completions are generated in Stage 7")?;
    Ok(())
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

fn load_or_new_plan(store: &Store, date: &PlanDate, context: &ContextRefs<'_>) -> Result<Plan> {
    if let Some(plan) = store.load_plan(date)? {
        return Ok(plan);
    }
    let timezone = timezone_from_clock(context)?;
    Ok(empty_plan(date.clone(), timezone))
}

fn empty_plan(date: PlanDate, timezone: TimeZoneName) -> Plan {
    Plan {
        date,
        timezone,
        blocks: Vec::new(),
    }
}

fn replace_block(store: &Store, plan: &mut Plan, index: usize, block: Block) -> Result<()> {
    plan.blocks[index] = block;
    store.set_plan(plan, HistoryPolicy::Preserve)?;
    Ok(())
}

fn insert_block(store: &Store, plan: &mut Plan, block: Block) -> Result<()> {
    plan.blocks.push(block);
    store.set_plan(plan, HistoryPolicy::Preserve)?;
    Ok(())
}

fn load_required(store: &Store, date: &PlanDate) -> Result<Plan> {
    store
        .load_plan(date)?
        .ok_or_else(|| Error::NotFound(format!("plan for {date}")))
}

fn reconciled_plan(context: &ContextRefs<'_>, date: &PlanDate) -> Result<Plan> {
    let mut plan = load_required(context.store, date)?;
    let now = context.clock.now().timestamp();
    let updates = reconcile_overdue(&plan, now, context.policy.grace())?;
    if updates.is_empty() {
        return Ok(plan);
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
    context.store.set_plan(&plan, HistoryPolicy::Preserve)?;
    Ok(plan)
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
        for (event, scheduled_at) in [
            (Event::Notify, notify_at),
            (Event::Start, start),
            (Event::End, end),
        ] {
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

fn write_read_array<T>(out: &mut dyn Write, json: bool, values: &[T]) -> Result<()>
where
    T: Serialize,
{
    if json {
        serde_json::to_writer_pretty(&mut *out, values)?;
        writeln!(out)?;
    } else if values.is_empty() {
        writeln!(out, "[]")?;
    } else {
        writeln!(out, "{} item(s)", values.len())?;
    }
    Ok(())
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

fn activate_block(
    context: &ContextRefs<'_>,
    plan: &mut Plan,
    index: usize,
    do_notify: bool,
    run: bool,
    log_line: &mut String,
) -> Result<()> {
    plan.blocks[index].status = Status::Active;
    if do_notify {
        log_notification_result(
            send_notification(context, &plan.blocks[index]),
            "",
            log_line,
        );
    }
    if run {
        log_line.push_str(" activated run-deferred");
    } else {
        log_line.push_str(" activated");
    }
    context.store.set_plan(plan, HistoryPolicy::Preserve)?;
    Ok(())
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
