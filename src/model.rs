//! Pure plan schema, validation, and schedule revision logic.

use std::{
    collections::HashSet,
    fmt::{self, Write as _},
    str::FromStr,
};

use jiff::{civil::Date, tz::TimeZone};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

const SECONDS_PER_DAY: u32 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub date: PlanDate,
    pub timezone: TimeZoneName,
    #[serde(rename = "block", default)]
    pub blocks: Vec<Block>,
}

impl Plan {
    /// Parses a TOML plan and validates its domain invariants.
    ///
    /// # Errors
    ///
    /// Returns an exit-code-2 `PlanError` if TOML parsing fails or the plan violates schema
    /// invariants.
    pub fn from_toml(input: &str) -> Result<Self, PlanError> {
        Self::from_toml_with_default(input, Lead::from_seconds_const(300))
    }

    /// Parses a TOML plan with a specified default notification lead.
    ///
    /// # Errors
    ///
    /// Returns an exit-code-2 `PlanError` if TOML parsing fails or the plan violates schema
    /// invariants.
    pub fn from_toml_with_default(input: &str, default_lead: Lead) -> Result<Self, PlanError> {
        let raw = toml::from_str::<RawPlan>(input)?;
        let plan = Self::from_raw(raw, default_lead)?;
        plan.validate()?;
        Ok(plan)
    }

    /// Converts raw deserialized plan into a domain plan using a default notify lead.
    fn from_raw(raw: RawPlan, default_lead: Lead) -> Result<Self, ValidationError> {
        let blocks = raw
            .blocks
            .into_iter()
            .map(|raw_block| Block::from_raw(raw_block, default_lead))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            date: raw.date,
            timezone: raw.timezone,
            blocks,
        })
    }

    /// Serializes a validated plan to TOML.
    ///
    /// # Errors
    ///
    /// Returns an exit-code-2 `PlanError` if validation or TOML serialization fails.
    pub fn to_toml(&self) -> Result<String, PlanError> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    /// Validates cross-field plan invariants that serde cannot express.
    ///
    /// # Errors
    ///
    /// Returns the first validation error with enough block context for a CLI diagnostic.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut seen_ids = HashSet::with_capacity(self.blocks.len());
        for block in &self.blocks {
            if !seen_ids.insert(block.id.clone()) {
                return Err(ValidationError::DuplicateId {
                    id: block.id.clone(),
                });
            }

            match block.span {
                Span::End(end)
                    if end.seconds_since_midnight() <= block.start.seconds_since_midnight() =>
                {
                    return Err(ValidationError::EndNotAfterStart {
                        id: block.id.clone(),
                    });
                }
                Span::Duration(duration) => {
                    let end = block.start.seconds_since_midnight() + duration.as_seconds();
                    if end > SECONDS_PER_DAY {
                        return Err(ValidationError::EndPastDay {
                            id: block.id.clone(),
                        });
                    }
                }
                Span::End(_) => {}
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn schedule_revs(&self) -> Vec<(BlockId, ScheduleRev)> {
        let mut revs = self
            .blocks
            .iter()
            .map(|block| (block.id.clone(), block.schedule_rev()))
            .collect::<Vec<_>>();
        revs.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        revs
    }
}

impl TryFrom<RawPlan> for Plan {
    type Error = ValidationError;

    fn try_from(raw: RawPlan) -> Result<Self, Self::Error> {
        Self::from_raw(raw, Lead::from_seconds_const(300))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlan {
    date: PlanDate,
    timezone: TimeZoneName,
    #[serde(rename = "block", default)]
    blocks: Vec<RawBlock>,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("invalid plan TOML: {0}")]
    TomlRead(#[from] toml::de::Error),
    #[error("invalid plan: {0}")]
    Validation(#[from] ValidationError),
    #[error("failed to write plan TOML: {0}")]
    TomlWrite(#[from] toml::ser::Error),
}

impl PlanError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::TomlRead(_) | Self::Validation(_) | Self::TomlWrite(_) => 2,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("duplicate block id `{id}`")]
    DuplicateId { id: BlockId },
    #[error("block `{id}` must set exactly one of end or duration")]
    MissingEndOrDuration { id: BlockId },
    #[error("block `{id}` must not set both end and duration")]
    BothEndAndDuration { id: BlockId },
    #[error("block `{id}` must end after it starts")]
    EndNotAfterStart { id: BlockId },
    #[error("block `{id}` duration crosses the end of the day")]
    EndPastDay { id: BlockId },
    #[error("block `{id}` run argv must contain argv[0]")]
    EmptyRun { id: BlockId },
}

impl ValidationError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::DuplicateId { .. }
            | Self::MissingEndOrDuration { .. }
            | Self::BothEndAndDuration { .. }
            | Self::EndNotAfterStart { .. }
            | Self::EndPastDay { .. }
            | Self::EmptyRun { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub title: String,
    pub start: ClockTime,
    pub span: Span,
    pub notify: Lead,
    pub tags: Vec<String>,
    pub status: Status,
    pub run: Option<Run>,
}

impl Block {
    #[must_use]
    pub fn schedule_rev(&self) -> ScheduleRev {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ccplan-schedule-rev-v1\0");
        update_hash_field(&mut hasher, "id", self.id.as_str());
        update_hash_field(
            &mut hasher,
            "start",
            &self.start.seconds_since_midnight().to_string(),
        );
        update_hash_field(&mut hasher, "end", &self.resolved_end_seconds().to_string());
        update_hash_field(&mut hasher, "notify", &self.notify.as_seconds().to_string());
        ScheduleRev::from_hash(hasher.finalize())
    }

    fn resolved_end_seconds(&self) -> u32 {
        self.span.resolved_end_seconds(self.start)
    }
}

impl Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        RawBlock::from(self).serialize(serializer)
    }
}

impl TryFrom<RawBlock> for Block {
    type Error = ValidationError;

    fn try_from(raw: RawBlock) -> Result<Self, Self::Error> {
        Self::from_raw(raw, Lead::from_seconds_const(300))
    }
}

impl Block {
    /// Converts a `RawBlock` to a Block, using the provided default lead if notify was omitted.
    fn from_raw(raw: RawBlock, default_lead: Lead) -> Result<Self, ValidationError> {
        let span = match (raw.end, raw.duration) {
            (Some(_), Some(_)) => {
                return Err(ValidationError::BothEndAndDuration { id: raw.id });
            }
            (None, None) => {
                return Err(ValidationError::MissingEndOrDuration { id: raw.id });
            }
            (Some(end), None) => Span::End(end),
            (None, Some(duration)) => Span::Duration(duration),
        };
        let run = raw
            .run
            .map(Run::new)
            .transpose()
            .map_err(|_| ValidationError::EmptyRun { id: raw.id.clone() })?;

        Ok(Self {
            id: raw.id,
            title: raw.title,
            start: raw.start,
            span,
            notify: raw.notify.unwrap_or(default_lead),
            tags: raw.tags,
            status: raw.status,
            run,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    End(ClockTime),
    Duration(DurationSpec),
}

impl Span {
    #[must_use]
    pub const fn resolved_end_seconds(&self, start: ClockTime) -> u32 {
        match self {
            Self::End(end) => end.seconds_since_midnight(),
            Self::Duration(duration) => start.seconds_since_midnight() + duration.as_seconds(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run(Vec<String>);

impl Run {
    /// Creates a non-empty argv vector.
    ///
    /// # Errors
    ///
    /// Returns an error when `argv[0]` is missing or empty.
    pub fn new(argv: Vec<String>) -> Result<Self, FieldParseError> {
        if argv.first().is_none_or(String::is_empty) {
            return Err(FieldParseError::Run);
        }

        Ok(Self(argv))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    id: BlockId,
    title: String,
    start: ClockTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<ClockTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration: Option<DurationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify: Option<Lead>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<Vec<String>>,
}

impl From<&Block> for RawBlock {
    fn from(block: &Block) -> Self {
        let (end, duration) = match block.span {
            Span::End(end) => (Some(end), None),
            Span::Duration(duration) => (None, Some(duration)),
        };

        Self {
            id: block.id.clone(),
            title: block.title.clone(),
            start: block.start,
            end,
            duration,
            notify: Some(block.notify),
            tags: block.tags.clone(),
            status: block.status,
            run: block.run.as_ref().map(|argv| argv.as_slice().to_vec()),
        }
    }
}

fn update_hash_field(hasher: &mut blake3::Hasher, name: &str, value: &str) {
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockId(String);

impl BlockId {
    /// Creates a block id after validating its portable character set.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is empty or contains characters that cannot safely participate in
    /// scheduler identity strings.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldParseError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(FieldParseError::BlockId { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for BlockId {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for BlockId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BlockId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDate(Date);

impl PlanDate {
    #[must_use]
    pub const fn as_jiff_date(&self) -> Date {
        self.0
    }

    #[must_use]
    pub const fn from_jiff_date(date: Date) -> Self {
        Self(date)
    }
}

impl fmt::Display for PlanDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for PlanDate {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<Date>()
            .map(Self)
            .map_err(|_| FieldParseError::Date {
                value: value.to_owned(),
            })
    }
}

impl Serialize for PlanDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZoneName(String);

impl TimeZoneName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolves this validated IANA name to a `jiff` time zone.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying time zone database can no longer load the name.
    pub fn to_time_zone(&self) -> Result<TimeZone, FieldParseError> {
        TimeZone::get(&self.0).map_err(|_| FieldParseError::TimeZone {
            value: self.0.clone(),
        })
    }
}

impl fmt::Display for TimeZoneName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TimeZoneName {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let timezone = TimeZone::get(value).map_err(|_| FieldParseError::TimeZone {
            value: value.to_owned(),
        })?;
        let canonical = timezone.iana_name().unwrap_or(value);

        Ok(Self(canonical.to_owned()))
    }
}

impl Serialize for TimeZoneName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TimeZoneName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockTime {
    minutes_since_midnight: u16,
}

impl ClockTime {
    /// Creates a wall-clock time from minutes since local midnight.
    ///
    /// # Errors
    ///
    /// Returns an error if the minute count is outside a single day.
    pub fn from_minutes_since_midnight(minutes: u16) -> Result<Self, FieldParseError> {
        if minutes >= 24 * 60 {
            return Err(FieldParseError::ClockTime {
                value: minutes.to_string(),
            });
        }

        Ok(Self {
            minutes_since_midnight: minutes,
        })
    }

    #[must_use]
    pub const fn minutes_since_midnight(self) -> u16 {
        self.minutes_since_midnight
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ClockTime validates the minute count is within a single day"
    )]
    pub const fn hour(self) -> i8 {
        (self.minutes_since_midnight / 60) as i8
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ClockTime validates the minute count is within a single day"
    )]
    pub const fn minute(self) -> i8 {
        (self.minutes_since_midnight % 60) as i8
    }

    #[must_use]
    pub const fn seconds_since_midnight(self) -> u32 {
        self.minutes_since_midnight as u32 * 60
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:02}:{:02}",
            self.minutes_since_midnight / 60,
            self.minutes_since_midnight % 60
        )
    }
}

impl FromStr for ClockTime {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((hour, minute)) = value.split_once(':') else {
            return Err(FieldParseError::ClockTime {
                value: value.to_owned(),
            });
        };
        if hour.len() != 2 || minute.len() != 2 {
            return Err(FieldParseError::ClockTime {
                value: value.to_owned(),
            });
        }

        let hour = hour
            .parse::<u16>()
            .map_err(|_| FieldParseError::ClockTime {
                value: value.to_owned(),
            })?;
        let minute = minute
            .parse::<u16>()
            .map_err(|_| FieldParseError::ClockTime {
                value: value.to_owned(),
            })?;

        if hour >= 24 || minute >= 60 {
            return Err(FieldParseError::ClockTime {
                value: value.to_owned(),
            });
        }

        Self::from_minutes_since_midnight(hour * 60 + minute)
    }
}

impl Serialize for ClockTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationSpec {
    seconds: u32,
}

impl DurationSpec {
    /// Creates a positive duration from seconds.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or longer-than-a-day durations.
    pub fn from_seconds(seconds: u32) -> Result<Self, FieldParseError> {
        if seconds == 0 || seconds > SECONDS_PER_DAY {
            return Err(FieldParseError::Duration {
                value: seconds.to_string(),
            });
        }

        Ok(Self { seconds })
    }

    /// Const constructor for compile-time config defaults. Returns `None` for invalid values.
    #[must_use]
    pub const fn from_seconds_const(seconds: u32) -> Option<Self> {
        if seconds == 0 || seconds > SECONDS_PER_DAY {
            None
        } else {
            Some(Self { seconds })
        }
    }

    #[must_use]
    pub const fn as_seconds(self) -> u32 {
        self.seconds
    }
}

impl fmt::Display for DurationSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&format_seconds(self.seconds))
    }
}

impl FromStr for DurationSpec {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_seconds(parse_duration_seconds(value)?)
    }
}

impl Serialize for DurationSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lead {
    seconds: u32,
}

impl Lead {
    /// Creates a notification lead from seconds.
    ///
    /// # Errors
    ///
    /// Returns an error for longer-than-a-day lead durations.
    pub fn from_seconds(seconds: u32) -> Result<Self, FieldParseError> {
        if seconds > SECONDS_PER_DAY {
            return Err(FieldParseError::Duration {
                value: seconds.to_string(),
            });
        }

        Ok(Self { seconds })
    }

    /// Const constructor for compile-time config defaults.
    #[must_use]
    pub const fn from_seconds_const(seconds: u32) -> Self {
        Self { seconds }
    }

    #[must_use]
    pub const fn as_seconds(self) -> u32 {
        self.seconds
    }
}

impl fmt::Display for Lead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&format_seconds(self.seconds))
    }
}

impl FromStr for Lead {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_seconds(parse_duration_seconds(value)?)
    }
}

impl Serialize for Lead {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Lead {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

fn parse_duration_seconds(value: &str) -> Result<u32, FieldParseError> {
    if value.is_empty() {
        return Err(FieldParseError::Duration {
            value: value.to_owned(),
        });
    }

    let mut cursor = 0;
    let mut total = 0_u32;
    let mut last_unit_rank = 4_u8;
    let bytes = value.as_bytes();
    while cursor < bytes.len() {
        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if digits_start == cursor || cursor == bytes.len() {
            return Err(FieldParseError::Duration {
                value: value.to_owned(),
            });
        }

        let amount =
            value[digits_start..cursor]
                .parse::<u32>()
                .map_err(|_| FieldParseError::Duration {
                    value: value.to_owned(),
                })?;
        let (rank, multiplier) = match bytes[cursor] {
            b'h' => (3, 60 * 60),
            b'm' => (2, 60),
            b's' => (1, 1),
            _ => {
                return Err(FieldParseError::Duration {
                    value: value.to_owned(),
                });
            }
        };
        if rank >= last_unit_rank {
            return Err(FieldParseError::Duration {
                value: value.to_owned(),
            });
        }
        last_unit_rank = rank;
        total = total
            .checked_add(amount.saturating_mul(multiplier))
            .ok_or_else(|| FieldParseError::Duration {
                value: value.to_owned(),
            })?;
        cursor += 1;
    }

    Ok(total)
}

fn format_seconds(seconds: u32) -> String {
    if seconds == 0 {
        return "0m".to_owned();
    }

    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    let mut output = String::new();
    if hours > 0 {
        write!(&mut output, "{hours}h").expect("writing to a String cannot fail");
    }
    if minutes > 0 {
        write!(&mut output, "{minutes}m").expect("writing to a String cannot fail");
    }
    if seconds > 0 {
        write!(&mut output, "{seconds}s").expect("writing to a String cannot fail");
    }
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Pending,
    Active,
    Done,
    Skipped,
    Missed,
    Expired,
}

impl Status {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Skipped | Self::Missed | Self::Expired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduleRev(String);

impl ScheduleRev {
    fn from_hash(hash: blake3::Hash) -> Self {
        Self(hash.to_hex()[..16].to_owned())
    }

    /// Parses a schedule revision embedded in a trigger.
    ///
    /// # Errors
    ///
    /// Returns an error when the rev is not the 16-character lowercase hex form ccplan emits.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldParseError> {
        let value = value.into();
        if value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value))
        } else {
            Err(FieldParseError::ScheduleRev { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScheduleRev {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ScheduleRev {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FieldParseError {
    #[error("invalid block id `{value}`")]
    BlockId { value: String },
    #[error("invalid plan date `{value}`")]
    Date { value: String },
    #[error("invalid time zone `{value}`")]
    TimeZone { value: String },
    #[error("invalid clock time `{value}`")]
    ClockTime { value: String },
    #[error("invalid duration `{value}`")]
    Duration { value: String },
    #[error("invalid schedule rev `{value}`")]
    ScheduleRev { value: String },
    #[error("run argv must contain argv[0]")]
    Run,
}

#[cfg(test)]
mod timezone_name_tests {
    use super::*;

    #[test]
    fn time_zone_lookup_reports_invalid_private_name() {
        let timezone = TimeZoneName("Not/AZone".to_owned());

        assert!(matches!(
            timezone.to_time_zone(),
            Err(FieldParseError::TimeZone { value }) if value == "Not/AZone"
        ));
    }

    #[test]
    fn test_raw_try_from_and_const_constructors() {
        assert_eq!(Lead::from_seconds_const(300).as_seconds(), 300);

        assert_eq!(
            DurationSpec::from_seconds_const(1800).unwrap().as_seconds(),
            1800
        );
        assert!(DurationSpec::from_seconds_const(0).is_none());
        assert!(DurationSpec::from_seconds_const(86401).is_none());

        let raw_block = RawBlock {
            id: BlockId::new("focus-1").unwrap(),
            title: "Focus time".to_owned(),
            start: "11:00".parse().unwrap(),
            end: Some("11:30".parse().unwrap()),
            duration: None,
            notify: None,
            tags: vec![],
            status: Status::Pending,
            run: None,
        };
        let block = Block::try_from(raw_block.clone()).unwrap();
        assert_eq!(block.id.as_str(), "focus-1");

        let raw_plan = RawPlan {
            date: "2026-06-08".parse().unwrap(),
            timezone: "Asia/Kolkata".parse().unwrap(),
            blocks: vec![raw_block],
        };
        let plan = Plan::try_from(raw_plan).unwrap();
        assert_eq!(plan.blocks.len(), 1);
    }
}
