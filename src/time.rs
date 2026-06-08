//! Time-zone aware resolution and injectable clock abstractions.

use jiff::{
    SignedDuration, Timestamp, Zoned,
    civil::{DateTime, Time},
};
use thiserror::Error;

use crate::model::{Block, ClockTime, FieldParseError, Plan, PlanDate, Span, TimeZoneName};

#[derive(Debug, Error)]
pub enum TimeError {
    #[error(transparent)]
    TimeZone(#[from] FieldParseError),
    #[error("failed to resolve local time: {0}")]
    Resolve(#[from] jiff::Error),
}

/// Resolves a local wall-clock time on a plan date into an absolute timestamp.
///
/// Jiff's `DateTime::to_zoned` uses the Compatible ambiguity strategy: gaps move forward to the
/// next real civil time, and folds choose the earlier occurrence. That matches OS timer behavior
/// most users expect around DST transitions.
///
/// # Errors
///
/// Returns an error if the validated time zone cannot be loaded or the resolved timestamp is outside
/// Jiff's supported range.
pub fn resolve(
    date: &PlanDate,
    timezone: &TimeZoneName,
    wallclock: ClockTime,
) -> Result<Timestamp, TimeError> {
    let time = Time::new(wallclock.hour(), wallclock.minute(), 0, 0)?;
    let datetime = DateTime::from_parts(date.as_jiff_date(), time);
    let zoned = datetime.to_zoned(timezone.to_time_zone()?)?;

    Ok(zoned.timestamp())
}

pub(crate) fn resolve_block_start(plan: &Plan, block: &Block) -> Result<Timestamp, TimeError> {
    resolve(&plan.date, &plan.timezone, block.start)
}

pub(crate) fn resolve_block_end(plan: &Plan, block: &Block) -> Result<Timestamp, TimeError> {
    match block.span {
        Span::End(end) => resolve(&plan.date, &plan.timezone, end),
        Span::Duration(duration) => {
            let start = resolve_block_start(plan, block)?;
            let duration = SignedDuration::from_secs(i64::from(duration.as_seconds()));
            Ok(start.checked_add(duration)?)
        }
    }
}

pub trait Clock {
    fn now(&self) -> Zoned;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn now(&self) -> Zoned {
        Zoned::now()
    }
}

#[cfg(any(test, feature = "test-fakes"))]
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Zoned,
}

#[cfg(any(test, feature = "test-fakes"))]
impl FixedClock {
    #[must_use]
    pub fn new(now: Zoned) -> Self {
        Self { now }
    }
}

#[cfg(any(test, feature = "test-fakes"))]
impl Clock for FixedClock {
    fn now(&self) -> Zoned {
        self.now.clone()
    }
}
