#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::process::ExitCode;

use clap::Parser;

// Main is only process plumbing; testable behavior starts at ccplan::run.
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
