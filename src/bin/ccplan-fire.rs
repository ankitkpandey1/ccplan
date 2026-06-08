#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::process::ExitCode;

use clap::Parser;

// Windows Task Scheduler uses this GUI-subsystem wrapper for fire invocations.
#[cfg_attr(coverage_nightly, coverage(off))]
fn main() -> ExitCode {
    let cli = ccplan::cli::Cli::parse();

    match ccplan::run(cli, std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(error.exit_code())
        }
    }
}
