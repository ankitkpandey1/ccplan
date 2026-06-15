//! Unsupported-platform scheduler placeholder.
//!
//! These stubs only ever return `Unavailable`; they perform no real work but are still IO-boundary
//! placeholders, so each carries a fn-level `coverage(off)` rather than a module-scope exclusion.

use crate::{
    context::{Scheduler, SchedulerError},
    platform::DoctorCheck,
    store::TriggerRecord,
};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeScheduler;

#[cfg_attr(coverage_nightly, coverage(off))]
impl NativeScheduler {
    pub(crate) fn new() -> Result<Self, SchedulerError> {
        Ok(Self)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl Scheduler for NativeScheduler {
    fn prepare(&self) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    fn add(&self, _trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    fn remove(&self, _backend_id: &str) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        Err(SchedulerError::Unavailable)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn doctor_check() -> DoctorCheck {
    DoctorCheck::error(
        "scheduler",
        "this operating system has no ccplan native scheduler backend",
        "use Linux, macOS, or Windows",
    )
}
