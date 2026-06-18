//! Command-line parsing surface.

use clap::{Args, Parser, Subcommand, ValueEnum};
use jiff::Timestamp;

use crate::{
    lifecycle::Event,
    model::{BlockId, ClockTime, DurationSpec, Lead, PlanDate, ScheduleRev},
};

/// Parsed `ccplan` command line.
#[derive(Debug, Parser)]
#[command(
    name = "ccplan",
    version,
    about = "Agent-authorable cross-platform CLI day planner"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Set(SetArgs),
    Add(AddArgs),
    Remind(RemindArgs),
    Edit(EditArgs),
    Rm(BlockTarget),
    Done(BlockTarget),
    Skip(BlockTarget),
    Snooze(SnoozeArgs),
    Clear(ClearArgs),
    Show(ReadArgs),
    Now(ReadArgs),
    Next(ReadArgs),
    Agenda(AgendaArgs),
    Watch(WatchArgs),
    Serve(ServeArgs),
    Apply(ApplyArgs),
    Diff(DiffArgs),
    Approve(ApproveArgs),
    Materialize(MaterializeArgs),
    Fire(FireArgs),
    #[command(hide = true)]
    Roll,
    Log(LogArgs),
    Template(TemplateArgs),
    Status,
    Doctor,
    Completions(CompletionsArgs),
    Mcp(McpArgs),
    /// Open the Cockpit desktop app.
    Gui,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub override_history: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub id: Option<BlockId>,
    #[arg(long)]
    pub title: String,
    #[arg(long)]
    pub start: ClockTime,
    #[arg(long)]
    pub end: Option<ClockTime>,
    #[arg(long)]
    pub duration: Option<DurationSpec>,
    #[arg(long)]
    pub notify: Option<Lead>,
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
    #[arg(long, num_args = 1.., value_name = "ARGV")]
    pub run: Vec<String>,
    #[arg(long)]
    pub every: Option<String>,
    #[arg(long)]
    pub until: Option<PlanDate>,
    #[arg(long)]
    pub count: Option<u32>,
    #[arg(long, value_delimiter = ',')]
    pub after: Vec<BlockId>,
    /// Retry policy as COUNT:BACKOFF, e.g. `3:30s`.
    #[arg(long)]
    pub retry: Option<String>,
    #[arg(long = "expect-by")]
    pub expect_by: Option<DurationSpec>,
}

#[derive(Debug, Args)]
pub struct RemindArgs {
    /// Reminder text, shown in the notification.
    pub text: String,
    /// Fire this long from now, e.g. `1h`, `30m`, `1h30m` (max 24h).
    #[arg(long = "in")]
    pub fire_in: DurationSpec,
    /// Override the auto-slugged block id.
    #[arg(long)]
    pub id: Option<BlockId>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    pub id: BlockId,
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub start: Option<ClockTime>,
    #[arg(long)]
    pub end: Option<ClockTime>,
    #[arg(long)]
    pub duration: Option<DurationSpec>,
    #[arg(long)]
    pub notify: Option<Lead>,
    #[arg(long, num_args = 1.., value_name = "ARGV")]
    pub run: Vec<String>,
}

#[derive(Debug, Args)]
pub struct BlockTarget {
    pub id: BlockId,
}

/// `snooze` pushes a non-terminal block later by a duration, then re-applies (close-the-loop:
/// react to a fire by sliding the block instead of editing absolute times by hand).
#[derive(Debug, Args)]
pub struct SnoozeArgs {
    pub id: BlockId,
    /// Shift the block this much later, e.g. `10m`, `1h` (must stay within the same day).
    #[arg(long = "by")]
    pub by: DurationSpec,
    #[arg(long)]
    pub date: Option<PlanDate>,
}

#[derive(Debug, Args)]
pub struct ClearArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub purge: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AgendaArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub json: bool,
}

/// `watch` renders the live agenda and refreshes it on a timer until interrupted — a read-only
/// dashboard over the same data as `agenda`, for leaving open in a terminal.
#[derive(Debug, Args)]
pub struct WatchArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    /// Refresh interval, e.g. `30s`, `1m`, `5m` (default `30s`, max 24h).
    #[arg(long = "every", default_value = "30s")]
    pub every: DurationSpec,
}

/// `serve` runs the optional resident daemon for reactive local automations.
#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    /// Agent name this serve process claims work for.
    #[arg(long)]
    pub agent: Option<String>,
    /// Poll interval, e.g. `30s`, `1m`, `5m` (default `30s`, max 24h).
    #[arg(long = "every", default_value = "30s")]
    pub every: DurationSpec,
    /// Run one polling tick and exit. Useful for tests and supervised invocations.
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
}

#[derive(Debug, Args)]
pub struct ApproveArgs {
    pub id: BlockId,
    #[arg(long)]
    pub date: Option<PlanDate>,
}

#[derive(Debug, Args)]
pub struct FireArgs {
    #[arg(long)]
    pub date: PlanDate,
    #[arg(long)]
    pub id: BlockId,
    #[arg(long)]
    pub event: Event,
    #[arg(long)]
    pub rev: ScheduleRev,
    #[arg(long)]
    pub at: Timestamp,
    #[arg(long, default_value = "0")]
    pub attempt: u32,
    #[arg(long)]
    pub dry_run: bool,
}

/// `materialize` expands recurring rules into concrete dated occurrences.
#[derive(Debug, Args)]
pub struct MaterializeArgs {
    /// Number of days ahead to materialize (default 14).
    #[arg(long, default_value = "14")]
    pub horizon: u32,
}

/// `log` reads the fire ledger — what the scheduler actually did — for close-the-loop re-planning.
#[derive(Debug, Args)]
pub struct LogArgs {
    /// Only show fires for this plan date.
    #[arg(long)]
    pub date: Option<PlanDate>,
    /// Only show fires at or after this RFC 3339 timestamp (e.g. what fired since you last looked).
    #[arg(long)]
    pub since: Option<Timestamp>,
    #[arg(long)]
    pub json: bool,
}

/// `template` saves and instantiates reusable day shapes — capture a good day once, then stamp it
/// onto any date (statuses reset to pending) and apply, instead of re-authoring it each morning.
#[derive(Debug, Args)]
pub struct TemplateArgs {
    #[command(subcommand)]
    pub command: TemplateCommand,
}

#[derive(Debug, Subcommand)]
pub enum TemplateCommand {
    /// Save the plan for a date as a named template.
    Save(TemplateNameArgs),
    /// List saved template names.
    List,
    /// Instantiate a template onto a date (resets statuses to pending) and apply it.
    Apply(TemplateApplyArgs),
}

#[derive(Debug, Args)]
pub struct TemplateNameArgs {
    pub name: String,
    #[arg(long)]
    pub date: Option<PlanDate>,
}

#[derive(Debug, Args)]
pub struct TemplateApplyArgs {
    pub name: String,
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long = "var", value_name = "NAME=VALUE")]
    pub vars: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    pub shell: Shell,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell")]
    Powershell,
}

#[derive(Debug, Args)]
pub struct McpArgs {}

impl std::fmt::Display for Shell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Powershell => "powershell",
        })
    }
}
