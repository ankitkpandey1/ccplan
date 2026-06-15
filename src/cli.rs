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
    Edit(EditArgs),
    Rm(BlockTarget),
    Done(BlockTarget),
    Skip(BlockTarget),
    Clear(ClearArgs),
    Show(ReadArgs),
    Now(ReadArgs),
    Next(ReadArgs),
    Agenda(AgendaArgs),
    Apply(ApplyArgs),
    Fire(FireArgs),
    Status,
    Doctor,
    Completions(CompletionsArgs),
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

#[derive(Debug, Args)]
pub struct ApplyArgs {
    #[arg(long)]
    pub date: Option<PlanDate>,
    #[arg(long)]
    pub dry_run: bool,
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
    #[arg(long)]
    pub dry_run: bool,
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
