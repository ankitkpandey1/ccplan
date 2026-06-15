//! Runtime context and side-effect traits for command orchestration.

#[cfg(any(test, feature = "test-fakes"))]
use std::{cell::RefCell, collections::BTreeMap};

use thiserror::Error;

use crate::{
    config::Config,
    lifecycle::{EndBehavior, LifecyclePolicy},
    store::{Store, TriggerRecord},
    time::Clock,
};

#[derive(Debug)]
pub struct Context<C, S, N> {
    pub store: Store,
    pub clock: C,
    pub scheduler: S,
    pub notifier: N,
    pub policy: LifecyclePolicy,
    pub config: Config,
}

impl<C, S, N> Context<C, S, N> {
    #[must_use]
    pub fn new(store: Store, clock: C, scheduler: S, notifier: N, config: Config) -> Self {
        let grace = jiff::SignedDuration::from_secs(i64::from(config.grace.as_seconds()));
        Self {
            store,
            clock,
            scheduler,
            notifier,
            policy: LifecyclePolicy::new(grace, EndBehavior::Expire),
            config,
        }
    }
}

pub struct ContextRefs<'a> {
    pub store: &'a Store,
    pub clock: &'a dyn Clock,
    pub scheduler: &'a dyn Scheduler,
    pub notifier: &'a dyn Notifier,
    pub policy: LifecyclePolicy,
    pub config: &'a Config,
}

impl<C, S, N> Context<C, S, N>
where
    C: Clock,
    S: Scheduler,
    N: Notifier,
{
    #[must_use]
    pub fn as_refs(&self) -> ContextRefs<'_> {
        ContextRefs {
            store: &self.store,
            clock: &self.clock,
            scheduler: &self.scheduler,
            notifier: &self.notifier,
            policy: self.policy,
            config: &self.config,
        }
    }
}

pub trait Scheduler {
    /// Prepares the native scheduler for a reconcile pass.
    ///
    /// # Errors
    ///
    /// Returns an error when scheduler prerequisites cannot be applied.
    fn prepare(&self) -> Result<(), SchedulerError>;

    /// Creates or replaces one owned backend trigger.
    ///
    /// # Errors
    ///
    /// Returns an error when the native scheduler cannot converge.
    fn add(&self, trigger: &TriggerRecord) -> Result<(), SchedulerError>;

    /// Removes one owned backend trigger by backend id.
    ///
    /// # Errors
    ///
    /// Returns an error when the native scheduler cannot remove the trigger.
    fn remove(&self, backend_id: &str) -> Result<(), SchedulerError>;

    /// Lists live backend trigger identities in ccplan's namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the native scheduler cannot be queried.
    fn list(&self) -> Result<Vec<String>, SchedulerError>;
}

pub trait Notifier {
    /// Checks whether desktop notifications appear usable in this process.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform notification backend is likely unavailable.
    fn check(&self) -> Result<(), NotifyError>;

    /// Sends one desktop notification.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform notification backend rejects the send.
    fn notify(&self, notification: &Notification) -> Result<(), NotifyError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("scheduler backend is unavailable on this platform")]
    Unavailable,
    #[error("scheduler operation failed: {0}")]
    Operation(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NotifyError {
    #[error("notifier backend is unavailable on this platform")]
    Unavailable,
    #[error("notification failed: {0}")]
    Operation(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableScheduler;

impl Scheduler for UnavailableScheduler {
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn prepare(&self) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn add(&self, _trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn remove(&self, _backend_id: &str) -> Result<(), SchedulerError> {
        Err(SchedulerError::Unavailable)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        Err(SchedulerError::Unavailable)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableNotifier;

impl Notifier for UnavailableNotifier {
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn check(&self) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn notify(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }
}

#[cfg(any(test, feature = "test-fakes"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerCall {
    Add(String),
    Remove(String),
}

#[cfg(any(test, feature = "test-fakes"))]
#[derive(Debug, Default)]
pub struct RecordingScheduler {
    triggers: RefCell<BTreeMap<String, TriggerRecord>>,
    calls: RefCell<Vec<SchedulerCall>>,
}

#[cfg(any(test, feature = "test-fakes"))]
impl RecordingScheduler {
    #[must_use]
    pub fn calls(&self) -> Vec<SchedulerCall> {
        self.calls.borrow().clone()
    }

    pub fn clear_calls(&self) {
        self.calls.borrow_mut().clear();
    }

    #[must_use]
    pub fn triggers(&self) -> Vec<TriggerRecord> {
        self.triggers.borrow().values().cloned().collect()
    }
}

#[cfg(any(test, feature = "test-fakes"))]
impl Scheduler for RecordingScheduler {
    fn prepare(&self) -> Result<(), SchedulerError> {
        Ok(())
    }

    fn add(&self, trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        self.calls
            .borrow_mut()
            .push(SchedulerCall::Add(trigger.backend_id.clone()));
        self.triggers
            .borrow_mut()
            .insert(trigger.backend_id.clone(), trigger.clone());
        Ok(())
    }

    fn remove(&self, backend_id: &str) -> Result<(), SchedulerError> {
        self.calls
            .borrow_mut()
            .push(SchedulerCall::Remove(backend_id.to_owned()));
        self.triggers.borrow_mut().remove(backend_id);
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        Ok(self.triggers.borrow().keys().cloned().collect())
    }
}

#[cfg(any(test, feature = "test-fakes"))]
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    notifications: RefCell<Vec<Notification>>,
}

#[cfg(any(test, feature = "test-fakes"))]
impl RecordingNotifier {
    #[must_use]
    pub fn notifications(&self) -> Vec<Notification> {
        self.notifications.borrow().clone()
    }
}

#[cfg(any(test, feature = "test-fakes"))]
impl Notifier for RecordingNotifier {
    fn check(&self) -> Result<(), NotifyError> {
        Ok(())
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.notifications.borrow_mut().push(notification.clone());
        Ok(())
    }
}
