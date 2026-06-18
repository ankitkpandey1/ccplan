//! Pure decision core for the opt-in `ccplan serve` daemon.

use std::collections::HashMap;

use crate::model::{BlockId, Plan, Status, WhenCondition};

/// Last condition marker observed by the resident serve loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeMemory {
    markers: HashMap<BlockId, String>,
}

impl ServeMemory {
    #[must_use]
    pub fn marker_for(&self, id: &BlockId) -> Option<&str> {
        self.markers.get(id).map(String::as_str)
    }
}

/// One polled condition result, supplied by the side-effecting serve boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionState {
    pub satisfied: bool,
    pub marker: Option<String>,
}

impl ConditionState {
    #[must_use]
    pub fn satisfied(marker: impl Into<String>) -> Self {
        Self {
            satisfied: true,
            marker: Some(marker.into()),
        }
    }

    #[must_use]
    pub const fn unsatisfied() -> Self {
        Self {
            satisfied: false,
            marker: None,
        }
    }

    fn active_marker(&self) -> Option<String> {
        self.satisfied.then(|| {
            self.marker
                .clone()
                .unwrap_or_else(|| "satisfied".to_owned())
        })
    }
}

/// A block whose condition crossed into a satisfied state this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactiveDecision {
    pub block_id: BlockId,
    pub condition: WhenCondition,
}

/// Result of one pure serve planning tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactivePlan {
    pub decisions: Vec<ReactiveDecision>,
    pub next_memory: ServeMemory,
}

/// Decides which reactive blocks should be armed on this serve tick.
///
/// A satisfied condition fires only when its marker is new for that block. Unsatisfied or missing
/// states clear the marker so a later satisfied transition can fire again.
#[must_use]
pub fn decide_reactive_triggers(
    plan: &Plan,
    states: &HashMap<BlockId, ConditionState>,
    memory: &ServeMemory,
) -> ReactivePlan {
    let mut decisions = Vec::new();
    let mut next_memory = ServeMemory::default();

    for block in &plan.blocks {
        let Some(condition) = &block.when else {
            continue;
        };
        if block.status != Status::Pending {
            continue;
        }
        let Some(marker) = states
            .get(&block.id)
            .and_then(ConditionState::active_marker)
        else {
            continue;
        };
        if memory.marker_for(&block.id) != Some(marker.as_str()) {
            decisions.push(ReactiveDecision {
                block_id: block.id.clone(),
                condition: condition.clone(),
            });
        }
        next_memory.markers.insert(block.id.clone(), marker);
    }

    ReactivePlan {
        decisions,
        next_memory,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::model::{
        Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, Run, Span, Status,
        TimeZoneName, WhenCondition,
    };

    use super::{ConditionState, ReactiveDecision, ServeMemory, decide_reactive_triggers};

    fn block(id: &str, status: Status, when: Option<WhenCondition>) -> Block {
        Block {
            id: BlockId::new(id).unwrap(),
            title: format!("Block {id}"),
            start: ClockTime::from_minutes_since_midnight(9 * 60).unwrap(),
            span: Span::Duration(DurationSpec::from_seconds(30 * 60).unwrap()),
            notify: Lead::from_seconds(0).unwrap(),
            tags: Vec::new(),
            status,
            run: None::<Run>,
            recurrence: None,
            origin: None,
            after: vec![],
            on_success: vec![],
            on_failure: vec![],
            on_missed: vec![],
            retry: None,
            expect_by: None,
            approval: None,
            when,
            agent: None,
        }
    }

    fn plan(blocks: Vec<Block>) -> Plan {
        Plan {
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            timezone: "UTC".parse::<TimeZoneName>().unwrap(),
            blocks,
        }
    }

    fn states(entries: &[(&str, ConditionState)]) -> HashMap<BlockId, ConditionState> {
        entries
            .iter()
            .map(|(id, state)| (BlockId::new(*id).unwrap(), state.clone()))
            .collect()
    }

    #[test]
    fn no_conditions_or_unsatisfied_states_fire_nothing() {
        let plan = plan(vec![
            block("plain", Status::Pending, None),
            block(
                "reactive",
                Status::Pending,
                Some(WhenCondition::FileExists("/tmp/ready".to_owned())),
            ),
        ]);
        let tick = decide_reactive_triggers(
            &plan,
            &states(&[("reactive", ConditionState::unsatisfied())]),
            &ServeMemory::default(),
        );

        assert!(tick.decisions.is_empty());
        assert!(
            tick.next_memory
                .marker_for(&BlockId::new("reactive").unwrap())
                .is_none()
        );
    }

    #[test]
    fn satisfied_condition_fires_once_until_it_resets() {
        let plan = plan(vec![block(
            "reactive",
            Status::Pending,
            Some(WhenCondition::FileExists("/tmp/ready".to_owned())),
        )]);
        let first = decide_reactive_triggers(
            &plan,
            &states(&[("reactive", ConditionState::satisfied("exists"))]),
            &ServeMemory::default(),
        );
        assert_eq!(
            first.decisions,
            vec![ReactiveDecision {
                block_id: BlockId::new("reactive").unwrap(),
                condition: WhenCondition::FileExists("/tmp/ready".to_owned()),
            }]
        );

        let repeated = decide_reactive_triggers(
            &plan,
            &states(&[("reactive", ConditionState::satisfied("exists"))]),
            &first.next_memory,
        );
        assert!(repeated.decisions.is_empty());

        let reset = decide_reactive_triggers(
            &plan,
            &states(&[("reactive", ConditionState::unsatisfied())]),
            &repeated.next_memory,
        );
        let refired = decide_reactive_triggers(
            &plan,
            &states(&[("reactive", ConditionState::satisfied("exists"))]),
            &reset.next_memory,
        );
        assert_eq!(refired.decisions.len(), 1);
    }

    #[test]
    fn changed_file_marker_fires_each_new_marker() {
        let plan = plan(vec![block(
            "input",
            Status::Pending,
            Some(WhenCondition::FileChanged("/tmp/input".to_owned())),
        )]);
        let first = decide_reactive_triggers(
            &plan,
            &states(&[("input", ConditionState::satisfied("mtime:1"))]),
            &ServeMemory::default(),
        );
        let second = decide_reactive_triggers(
            &plan,
            &states(&[("input", ConditionState::satisfied("mtime:2"))]),
            &first.next_memory,
        );

        assert_eq!(first.decisions.len(), 1);
        assert_eq!(second.decisions.len(), 1);
        assert_eq!(
            second
                .next_memory
                .marker_for(&BlockId::new("input").unwrap()),
            Some("mtime:2")
        );
    }

    #[test]
    fn satisfied_state_without_marker_uses_stable_default_marker() {
        let plan = plan(vec![block(
            "reactive",
            Status::Pending,
            Some(WhenCondition::CommandOk(vec!["/bin/true".to_owned()])),
        )]);
        let tick = decide_reactive_triggers(
            &plan,
            &states(&[(
                "reactive",
                ConditionState {
                    satisfied: true,
                    marker: None,
                },
            )]),
            &ServeMemory::default(),
        );

        assert_eq!(tick.decisions.len(), 1);
        assert_eq!(
            tick.next_memory
                .marker_for(&BlockId::new("reactive").unwrap()),
            Some("satisfied")
        );
    }

    #[test]
    fn non_pending_reactive_blocks_are_not_fired() {
        let plan = plan(vec![
            block(
                "active",
                Status::Active,
                Some(WhenCondition::CommandOk(vec!["/bin/true".to_owned()])),
            ),
            block(
                "done",
                Status::Done,
                Some(WhenCondition::CommandOk(vec!["/bin/true".to_owned()])),
            ),
        ]);
        let tick = decide_reactive_triggers(
            &plan,
            &states(&[
                ("active", ConditionState::satisfied("ok")),
                ("done", ConditionState::satisfied("ok")),
            ]),
            &ServeMemory::default(),
        );

        assert!(tick.decisions.is_empty());
        assert!(
            tick.next_memory
                .marker_for(&BlockId::new("active").unwrap())
                .is_none()
        );
    }
}
