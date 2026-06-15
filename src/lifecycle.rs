//! Pure lifecycle/fire decision logic.

use std::{fmt, str::FromStr};

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{
    model::{Block, BlockId, Plan, Status},
    time::{TimeError, resolve_block_end, resolve_block_start},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Event {
    Notify,
    Start,
    End,
}

impl fmt::Display for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Notify => "notify",
            Self::Start => "start",
            Self::End => "end",
        })
    }
}

impl FromStr for Event {
    type Err = EventParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "notify" => Ok(Self::Notify),
            "start" => Ok(Self::Start),
            "end" => Ok(Self::End),
            _ => Err(EventParseError {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventParseError {
    value: String,
}

impl fmt::Display for EventParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid event `{}`", self.value)
    }
}

impl std::error::Error for EventParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndBehavior {
    AutoDone,
    Expire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecyclePolicy {
    grace: SignedDuration,
    end_behavior: EndBehavior,
}

impl LifecyclePolicy {
    #[must_use]
    pub const fn new(grace: SignedDuration, end_behavior: EndBehavior) -> Self {
        Self {
            grace,
            end_behavior,
        }
    }

    #[must_use]
    pub const fn grace(self) -> SignedDuration {
        self.grace
    }

    const fn close_status(self) -> Status {
        match self.end_behavior {
            EndBehavior::AutoDone => Status::Done,
            EndBehavior::Expire => Status::Expired,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireDecision {
    NoOp,
    Notify,
    Activate { run: bool },
    MarkMissed,
    Close { status: Status },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusUpdate {
    pub id: BlockId,
    pub status: Status,
}

#[must_use]
pub fn decide_fire(
    block: &Block,
    event: Event,
    scheduled_at: Timestamp,
    now: Timestamp,
    policy: LifecyclePolicy,
) -> FireDecision {
    if block.status.is_terminal() {
        return FireDecision::NoOp;
    }

    match event {
        Event::Notify => {
            if is_overdue(scheduled_at, now, policy.grace) {
                FireDecision::NoOp
            } else {
                FireDecision::Notify
            }
        }
        Event::Start => decide_start(block, scheduled_at, now, policy.grace),
        Event::End => {
            if block.status == Status::Active {
                FireDecision::Close {
                    status: policy.close_status(),
                }
            } else {
                FireDecision::NoOp
            }
        }
    }
}

fn decide_start(
    block: &Block,
    scheduled_at: Timestamp,
    now: Timestamp,
    grace: SignedDuration,
) -> FireDecision {
    if block.status != Status::Pending {
        return FireDecision::NoOp;
    }

    if is_overdue(scheduled_at, now, grace) {
        FireDecision::MarkMissed
    } else {
        FireDecision::Activate {
            run: block.run.is_some(),
        }
    }
}

/// Computes status updates for blocks whose missed/expired windows elapsed while no trigger fired.
///
/// # Errors
///
/// Returns an error if one of the plan's local times cannot be resolved to an absolute timestamp.
pub fn reconcile_overdue(
    plan: &Plan,
    now: Timestamp,
    grace: SignedDuration,
) -> Result<Vec<StatusUpdate>, TimeError> {
    let mut updates = Vec::new();
    for block in &plan.blocks {
        if let Some(status) = reconcile_block(plan, block, now, grace)? {
            updates.push(StatusUpdate {
                id: block.id.clone(),
                status,
            });
        }
    }
    updates.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));

    Ok(updates)
}

fn reconcile_block(
    plan: &Plan,
    block: &Block,
    now: Timestamp,
    grace: SignedDuration,
) -> Result<Option<Status>, TimeError> {
    match block.status {
        Status::Pending => {
            let start = resolve_block_start(plan, block)?;
            Ok(is_overdue(start, now, grace).then_some(Status::Missed))
        }
        Status::Active => {
            let end = resolve_block_end(plan, block)?;
            Ok(is_overdue(end, now, grace).then_some(Status::Expired))
        }
        Status::Done | Status::Skipped | Status::Missed | Status::Expired => Ok(None),
    }
}

fn is_overdue(scheduled_at: Timestamp, now: Timestamp, grace: SignedDuration) -> bool {
    now.duration_since(scheduled_at) > grace
}
