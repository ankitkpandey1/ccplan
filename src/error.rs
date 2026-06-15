//! Typed application errors and documented process exit codes.

use std::io;

use thiserror::Error;

use crate::{
    context::SchedulerError,
    model::{BlockId, FieldParseError, PlanError},
    store::StoreError,
    time::TimeError,
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Field(#[from] FieldParseError),
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("usage error: {0}")]
    Usage(String),
    #[error("history conflict for terminal block `{id}`; use --override-history to replace it")]
    HistoryConflict { id: BlockId },
    #[error("automation refused: {0}")]
    AutomationRefused(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Store(error) => store_exit_code(error),
            Self::Plan(error) => code_to_u8(error.exit_code()),
            Self::Field(_) | Self::Time(_) | Self::Usage(_) => 2,
            Self::NotFound(_) => 3,
            Self::Scheduler(_) => 4,
            Self::AutomationRefused(_) => 5,
            Self::HistoryConflict { .. } => 6,
            Self::Io(_) | Self::Json(_) => 1,
        }
    }
}

const fn store_exit_code(error: &StoreError) -> u8 {
    code_to_u8(error.exit_code())
}

const fn code_to_u8(code: i32) -> u8 {
    match code {
        0 => 0,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 5,
        6 => 6,
        _ => 1,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io;

    use crate::{
        context::SchedulerError,
        model::{BlockId, Plan},
        store::StoreError,
        time::TimeError,
    };

    use super::{Error, code_to_u8};

    #[test]
    fn error_exit_codes_match_the_cli_contract() {
        assert_eq!(Error::Store(StoreError::Locked).exit_code(), 1);
        assert_eq!(
            Error::from(Plan::from_toml("date = 'bad'").unwrap_err()).exit_code(),
            2
        );
        assert_eq!(
            Error::from(BlockId::new("bad id").unwrap_err()).exit_code(),
            2
        );
        assert_eq!(
            Error::from(TimeError::from(
                "No/SuchZone"
                    .parse::<crate::model::TimeZoneName>()
                    .unwrap_err()
            ))
            .exit_code(),
            2
        );
        assert_eq!(Error::Usage("bad flag combo".to_owned()).exit_code(), 2);
        assert_eq!(Error::NotFound("block".to_owned()).exit_code(), 3);
        assert_eq!(Error::from(SchedulerError::Unavailable).exit_code(), 4);
        assert_eq!(
            Error::AutomationRefused("disabled".to_owned()).exit_code(),
            5
        );
        assert_eq!(
            Error::HistoryConflict {
                id: BlockId::new("done").unwrap(),
            }
            .exit_code(),
            6
        );
        assert_eq!(Error::from(io::Error::other("disk")).exit_code(), 1);
        assert_eq!(
            Error::from(serde_json::from_str::<serde_json::Value>("{").unwrap_err()).exit_code(),
            1
        );
        assert_eq!(code_to_u8(0), 0);
        assert_eq!(code_to_u8(3), 3);
        assert_eq!(code_to_u8(4), 4);
        assert_eq!(code_to_u8(5), 5);
        assert_eq!(code_to_u8(6), 6);
        assert_eq!(code_to_u8(99), 1);
    }
}
