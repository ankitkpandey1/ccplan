//! Library entrypoint for the ccplan CLI.

#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, allow(unused_features))]
#![warn(clippy::pedantic)]

use std::{io::Write, path::PathBuf};

pub mod cli;
#[cfg(test)]
mod cli_command;
mod commands;
pub mod config;
pub mod context;
pub mod error;
pub mod lifecycle;
pub mod mcp;
pub mod model;
mod platform;
pub mod store;
pub mod time;

use config::Config;
use context::Context;
use error::Result;
use platform::{NativeNotifier, NativeScheduler};
use store::Store;
use time::SystemClock;

/// Runs a parsed `ccplan` invocation.
///
/// # Errors
///
/// Returns an error if command execution fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the parsed CLI is owned at the application boundary and will grow command payloads"
)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn run<W>(cli: cli::Cli, mut out: W) -> Result<()>
where
    W: Write,
{
    let store = runtime_store()?;
    let config = Config::load(&store).map_err(|e| error::Error::Usage(e.to_string()))?;

    #[cfg(feature = "test-fakes")]
    if std::env::var_os("CCPLAN_TEST_FAKE_BACKENDS").is_some() {
        return run_with_test_backends(cli, &mut out, store, config);
    }

    let context = Context::new(
        store,
        SystemClock,
        NativeScheduler::new()?,
        NativeNotifier,
        config,
    );
    run_with_context(cli, &mut out, &context)?;
    Ok(())
}

#[cfg(feature = "test-fakes")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn run_with_test_backends<W>(cli: cli::Cli, out: &mut W, store: Store, config: Config) -> Result<()>
where
    W: Write,
{
    use context::{RecordingNotifier, RecordingScheduler};
    use time::FixedClock;

    let now = std::env::var("CCPLAN_TEST_NOW")
        .unwrap_or_else(|_| "2099-01-01T08:00:00+00:00[UTC]".to_owned());
    let now = now
        .parse()
        .map_err(|error| error::Error::Usage(format!("invalid CCPLAN_TEST_NOW: {error}")))?;
    let context = Context::new(
        store,
        FixedClock::new(now),
        RecordingScheduler::default(),
        RecordingNotifier::default(),
        config,
    );
    run_with_context(cli, out, &context)
}

/// Runs a parsed invocation against an injected context.
///
/// # Errors
///
/// Returns an error if command execution fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the parsed CLI is owned at the application boundary"
)]
pub fn run_with_context<C, S, N, W>(
    cli: cli::Cli,
    out: &mut W,
    context: &Context<C, S, N>,
) -> Result<()>
where
    C: time::Clock,
    S: context::Scheduler,
    N: context::Notifier,
    W: Write,
{
    commands::dispatch(cli.command, out, &context.as_refs())?;
    out.flush()?;
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn runtime_store() -> Result<Store> {
    if let Some(root) = std::env::var_os("CCPLAN_ROOT") {
        return Ok(Store::new(&PathBuf::from(root)));
    }
    Ok(Store::for_user()?)
}
