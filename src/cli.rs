//! Command-line parsing surface.

use clap::Parser;

/// Parsed `ccplan` command line.
#[derive(Debug, Parser)]
#[command(
    name = "ccplan",
    version,
    about = "Agent-authorable cross-platform CLI day planner"
)]
pub struct Cli {}
