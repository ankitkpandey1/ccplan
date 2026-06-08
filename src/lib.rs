//! Library entrypoint for the ccplan CLI.

#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, allow(unused_features))]
#![warn(clippy::pedantic)]

use std::io::Write;

pub mod cli;
pub mod lifecycle;
pub mod model;
pub mod store;
pub mod time;

/// Runs a parsed `ccplan` invocation.
///
/// # Errors
///
/// Returns an error if writing to the provided output stream fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the parsed CLI is owned at the application boundary and will grow command payloads"
)]
pub fn run<W>(cli: cli::Cli, mut out: W) -> anyhow::Result<()>
where
    W: Write,
{
    let cli::Cli {} = cli;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use clap::Parser;

    #[test]
    fn run_accepts_minimal_cli() {
        let cli = crate::cli::Cli::parse_from(["ccplan"]);
        let mut output = Vec::new();

        crate::run(cli, &mut output).expect("minimal invocation should run");

        assert!(output.is_empty());
    }
}
