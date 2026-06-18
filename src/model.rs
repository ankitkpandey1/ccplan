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
        toml::to_string_pretty(self).map_err(PlanError::from)
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

/// Template store for recurring block rules; read from / written to `data/recurring.toml`.
///
/// Unlike [`Plan`], `RecurringRules` has no date — each block carries its own `recurrence`
/// field and is expanded into concrete dated occurrences by the materializer.
#[derive(Debug, Clone, Default)]
pub struct RecurringRules {
    pub timezone: Option<TimeZoneName>,
    pub blocks: Vec<Block>,
}

impl RecurringRules {
    /// Parses a `recurring.toml` file.
    ///
    /// # Errors
    ///
    /// Returns a `PlanError` if parsing or field validation fails.
    pub fn from_toml(input: &str) -> Result<Self, PlanError> {
        let raw: RawRecurringRules = toml::from_str(input)?;
        let blocks = raw
            .blocks
            .into_iter()
            .map(|rb| Block::from_raw(rb, Lead::from_seconds_const(0)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(PlanError::from)?;
        Ok(Self {
            timezone: raw.timezone,
            blocks,
        })
    }

    /// Serialises to TOML.
    ///
    /// # Errors
    ///
    /// Returns a `PlanError` if TOML serialization fails.
    pub fn to_toml(&self) -> Result<String, PlanError> {
        let raw = RawRecurringRulesOut {
            timezone: self.timezone.clone(),
            blocks: self.blocks.iter().map(RawBlock::from).collect(),
        };
        Ok(toml::to_string_pretty(&raw)?)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecurringRules {
    #[serde(default)]
    timezone: Option<TimeZoneName>,
    #[serde(rename = "block", default)]
    blocks: Vec<RawBlock>,
}

#[derive(Debug, Serialize)]
struct RawRecurringRulesOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<TimeZoneName>,
    #[serde(rename = "block")]
    blocks: Vec<RawBlock>,
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
    #[error("block `{id}` has `every` without `anchor`")]
    MissingAnchor { id: BlockId },
    #[error("block `{id}` has both `count` and `until`")]
    BothCountAndUntil { id: BlockId },
    #[error("block `{id}` has unrecognised weekday token `{token}`")]
    BadWeekday { id: BlockId, token: String },
    #[error("block `{id}` has invalid `when` condition `{value}`")]
    BadWhen { id: BlockId, value: String },
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
            | Self::EmptyRun { .. }
            | Self::MissingAnchor { .. }
            | Self::BothCountAndUntil { .. }
            | Self::BadWeekday { .. }
            | Self::BadWhen { .. } => 2,
        }
    }
}

/// Day of week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    #[serde(rename = "mon")]
    Monday,
    #[serde(rename = "tue")]
    Tuesday,
    #[serde(rename = "wed")]
    Wednesday,
    #[serde(rename = "thu")]
    Thursday,
    #[serde(rename = "fri")]
    Friday,
    #[serde(rename = "sat")]
    Saturday,
    #[serde(rename = "sun")]
    Sunday,
}

impl fmt::Display for Weekday {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Monday => "mon",
            Self::Tuesday => "tue",
            Self::Wednesday => "wed",
            Self::Thursday => "thu",
            Self::Friday => "fri",
            Self::Saturday => "sat",
            Self::Sunday => "sun",
        })
    }
}

impl FromStr for Weekday {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mon" => Ok(Self::Monday),
            "tue" => Ok(Self::Tuesday),
            "wed" => Ok(Self::Wednesday),
            "thu" => Ok(Self::Thursday),
            "fri" => Ok(Self::Friday),
            "sat" => Ok(Self::Saturday),
            "sun" => Ok(Self::Sunday),
            _ => Err(FieldParseError::RecurRule {
                value: value.to_owned(),
            }),
        }
    }
}

/// Recurrence rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurRule {
    Daily,
    Weekday,
    Weekend,
    Weekly(Vec<Weekday>),
    EveryNDays(u16),
    EveryNWeeks(u16),
}

/// End condition for a recurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurEnd {
    Until(PlanDate),
    Count(u32),
}

/// A recurrence definition attached to a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    pub rule: RecurRule,
    pub anchor: PlanDate,
    pub end: Option<RecurEnd>,
}

/// Provenance of a generated block instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub rule_id: BlockId,
    pub gen_hash: String,
}

/// Retry policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub count: u32,
    pub backoff: DurationSpec,
}

/// Approval gate on automated blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    Pending,
    Approved,
}

/// Reactive condition that the opt-in `serve` daemon can poll for a block.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WhenCondition {
    FileExists(String),
    FileChanged(String),
    CommandOk(Vec<String>),
}

impl WhenCondition {
    fn parse_parenthesized<'a>(value: &'a str, name: &str) -> Option<&'a str> {
        let rest = value.strip_prefix(name)?;
        let body = rest.strip_prefix('(')?.strip_suffix(')')?;
        Some(body.trim())
    }
}

impl fmt::Display for WhenCondition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileExists(path) => write!(formatter, "file_exists({path})"),
            Self::FileChanged(path) => write!(formatter, "file_changed({path})"),
            Self::CommandOk(argv) => write!(formatter, "command_ok({})", argv.join(" ")),
        }
    }
}

impl FromStr for WhenCondition {
    type Err = FieldParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(path) = Self::parse_parenthesized(value, "file_exists") {
            if path.is_empty() {
                return Err(FieldParseError::WhenCondition {
                    value: value.to_owned(),
                });
            }
            return Ok(Self::FileExists(path.to_owned()));
        }
        if let Some(path) = Self::parse_parenthesized(value, "file_changed") {
            if path.is_empty() {
                return Err(FieldParseError::WhenCondition {
                    value: value.to_owned(),
                });
            }
            return Ok(Self::FileChanged(path.to_owned()));
        }
        if let Some(raw_argv) = Self::parse_parenthesized(value, "command_ok") {
            let argv = raw_argv
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if argv.is_empty() {
                return Err(FieldParseError::WhenCondition {
                    value: value.to_owned(),
                });
            }
            return Ok(Self::CommandOk(argv));
        }
        Err(FieldParseError::WhenCondition {
            value: value.to_owned(),
        })
    }
}

/// Wire-format retry (backoff as a string).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRetry {
    count: u32,
    backoff: String,
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
    pub recurrence: Option<Recurrence>,
    pub origin: Option<Origin>,
    pub after: Vec<BlockId>,
    pub on_success: Vec<BlockId>,
    pub on_failure: Vec<BlockId>,
    pub on_missed: Vec<BlockId>,
    pub retry: Option<Retry>,
    pub expect_by: Option<DurationSpec>,
    pub approval: Option<Approval>,
    pub when: Option<WhenCondition>,
    pub agent: Option<String>,
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

        // Parse recurrence from every + anchor + until/count fields.
        let recurrence = if let Some(ref every_str) = raw.every {
            let anchor = raw
                .anchor
                .ok_or_else(|| ValidationError::MissingAnchor { id: raw.id.clone() })?;
            if raw.count.is_some() && raw.until.is_some() {
                return Err(ValidationError::BothCountAndUntil { id: raw.id });
            }
            // parse_every always returns FieldParseError::RecurRule { value: every_str }.
            let rule = crate::recurrence::parse_every(every_str).map_err(|_| {
                let value = every_str.to_owned();
                if let Some(rest) = value.strip_prefix("weekly:") {
                    // Find the first bad token in a weekly:X,Y,… pattern.
                    let bad = rest
                        .split(',')
                        .find(|t| t.parse::<Weekday>().is_err())
                        .unwrap_or(rest);
                    ValidationError::BadWeekday {
                        id: raw.id.clone(),
                        token: bad.to_owned(),
                    }
                } else {
                    // Non-weekday bad `every` value — report as BadWeekday with the raw token.
                    ValidationError::BadWeekday {
                        id: raw.id.clone(),
                        token: value,
                    }
                }
            })?;
            let end = match (raw.count, raw.until) {
                (Some(n), None) => Some(RecurEnd::Count(n)),
                (None, Some(d)) => Some(RecurEnd::Until(d)),
                _ => None,
            };
            Some(Recurrence { rule, anchor, end })
        } else {
            // anchor/count/until without every are silently ignored.
            None
        };

        // Parse retry
        let retry = raw
            .retry
            .map(|r| {
                r.backoff
                    .parse::<DurationSpec>()
                    .map(|backoff| Retry {
                        count: r.count,
                        backoff,
                    })
                    .map_err(|_| ValidationError::EmptyRun { id: raw.id.clone() })
            })
            .transpose()?;

        // run: Some -> set approval = Pending if not already set
        let approval = if run.is_some() && raw.approval.is_none() {
            Some(Approval::Pending)
        } else {
            raw.approval
        };
        let when = raw
            .when
            .map(|value| {
                value
                    .parse::<WhenCondition>()
                    .map_err(|_| ValidationError::BadWhen {
                        id: raw.id.clone(),
                        value,
                    })
            })
            .transpose()?;

        Ok(Self {
            id: raw.id,
            title: raw.title,
            start: raw.start,
            span,
            notify: raw.notify.unwrap_or(default_lead),
            tags: raw.tags,
            status: raw.status,
            run,
            recurrence,
            origin: raw.origin,
            after: raw.after,
            on_success: raw.on_success,
            on_failure: raw.on_failure,
            on_missed: raw.on_missed,
            retry,
            expect_by: raw.expect_by,
            approval,
            when,
            agent: raw.agent,
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
    // Recurrence flat fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    every: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor: Option<PlanDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until: Option<PlanDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    count: Option<u32>,
    // New fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Origin>,
    #[serde(default)]
    after: Vec<BlockId>,
    #[serde(default)]
    on_success: Vec<BlockId>,
    #[serde(default)]
    on_failure: Vec<BlockId>,
    #[serde(default)]
    on_missed: Vec<BlockId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry: Option<RawRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expect_by: Option<DurationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
}

impl From<&Block> for RawBlock {
    fn from(block: &Block) -> Self {
        let (end, duration) = match block.span {
            Span::End(end) => (Some(end), None),
            Span::Duration(duration) => (None, Some(duration)),
        };

        // Serialize recurrence back to flat fields
        let (every, anchor, until, count) = if let Some(ref rec) = block.recurrence {
            let every_str = recur_rule_to_every(&rec.rule);
            let (until_date, cnt) = match &rec.end {
                Some(RecurEnd::Until(d)) => (Some(d.clone()), None),
                Some(RecurEnd::Count(n)) => (None, Some(*n)),
                None => (None, None),
            };
            (Some(every_str), Some(rec.anchor.clone()), until_date, cnt)
        } else {
            (None, None, None, None)
        };

        let retry = block.retry.as_ref().map(|r| RawRetry {
            count: r.count,
            backoff: r.backoff.to_string(),
        });

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
            every,
            anchor,
            until,
            count,
            origin: block.origin.clone(),
            after: block.after.clone(),
            on_success: block.on_success.clone(),
            on_failure: block.on_failure.clone(),
            on_missed: block.on_missed.clone(),
            retry,
            expect_by: block.expect_by,
            approval: block.approval,
            when: block.when.as_ref().map(ToString::to_string),
            agent: block.agent.clone(),
        }
    }
}

/// Serializes a `RecurRule` back to the `every` string.
impl fmt::Display for RecurRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&recur_rule_to_every(self))
    }
}

fn recur_rule_to_every(rule: &RecurRule) -> String {
    match rule {
        RecurRule::Daily => "daily".to_owned(),
        RecurRule::Weekday => "weekday".to_owned(),
        RecurRule::Weekend => "weekend".to_owned(),
        RecurRule::Weekly(days) => {
            let joined = days
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("weekly:{joined}")
        }
        RecurRule::EveryNDays(n) => format!("{n}d"),
        RecurRule::EveryNWeeks(n) => format!("{n}w"),
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
    /// Returns for zero or longer-than-a-day durations.
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

    /// Canonical lowercase label used for human-readable display.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Missed => "missed",
            Self::Expired => "expired",
        }
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
    #[error("invalid recurrence rule `{value}`")]
    RecurRule { value: String },
    #[error("invalid `when` condition `{value}`")]
    WhenCondition { value: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recurrence::parse_every;

    // ── Weekday parse / display ──────────────────────────────────────────

    #[test]
    fn weekday_parse_and_display_all_seven() {
        let cases = [
            ("mon", Weekday::Monday),
            ("tue", Weekday::Tuesday),
            ("wed", Weekday::Wednesday),
            ("thu", Weekday::Thursday),
            ("fri", Weekday::Friday),
            ("sat", Weekday::Saturday),
            ("sun", Weekday::Sunday),
        ];
        for (s, day) in cases {
            assert_eq!(s.parse::<Weekday>().unwrap(), day);
            assert_eq!(day.to_string(), s);
            // Exercise serde Deserialize (round-trips through JSON).
            let serialized = serde_json::to_string(&day).unwrap();
            let deserialized: Weekday = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, day);
        }
        assert!("xyz".parse::<Weekday>().is_err());
        // Exercise serde error path for Weekday.
        assert!(serde_json::from_str::<Weekday>("\"bogus\"").is_err());
    }

    // ── RecurRule variants via parse_every ───────────────────────────────

    #[test]
    fn recur_rule_variants_parse_correctly() {
        assert_eq!(parse_every("daily").unwrap(), RecurRule::Daily);
        assert_eq!(parse_every("weekday").unwrap(), RecurRule::Weekday);
        assert_eq!(parse_every("weekend").unwrap(), RecurRule::Weekend);
        assert_eq!(
            parse_every("weekly:mon").unwrap(),
            RecurRule::Weekly(vec![Weekday::Monday])
        );
        assert_eq!(
            parse_every("weekly:mon,wed,fri").unwrap(),
            RecurRule::Weekly(vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday])
        );
        assert_eq!(parse_every("3d").unwrap(), RecurRule::EveryNDays(3));
        assert_eq!(parse_every("14d").unwrap(), RecurRule::EveryNDays(14));
        assert_eq!(parse_every("2w").unwrap(), RecurRule::EveryNWeeks(2));
        let err = parse_every("bogus").unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    // ── every without anchor ─────────────────────────────────────────────

    #[test]
    fn every_without_anchor_is_an_error() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "rec-1"
title = "Daily standup"
start = "09:00"
end   = "09:15"
every = "daily"
"#;
        let err = Plan::from_toml(toml).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("anchor"));
        assert!(matches!(
            err,
            PlanError::Validation(ValidationError::MissingAnchor { ref id })
                if id.as_str() == "rec-1"
        ));
    }

    // ── count + until both set ───────────────────────────────────────────

    #[test]
    fn count_and_until_both_set_is_an_error() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "rec-1"
title  = "Daily standup"
start  = "09:00"
end    = "09:15"
every  = "daily"
anchor = "2026-06-01"
count  = 5
until  = "2026-06-30"
"#;
        let err = Plan::from_toml(toml).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("until"));
        assert!(matches!(
            err,
            PlanError::Validation(ValidationError::BothCountAndUntil { ref id })
                if id.as_str() == "rec-1"
        ));
    }

    // ── bad weekday token ────────────────────────────────────────────────

    #[test]
    fn bad_weekday_token_is_an_error() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "rec-1"
title  = "Weekly meeting"
start  = "10:00"
end    = "10:30"
every  = "weekly:badday"
anchor = "2026-06-01"
"#;
        let err = Plan::from_toml(toml).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("badday"));
        assert!(matches!(
            err,
            PlanError::Validation(ValidationError::BadWeekday { ref id, ref token })
                if id.as_str() == "rec-1" && token == "badday"
        ));
    }

    // ── bad non-weekly every token ───────────────────────────────────────

    #[test]
    fn bad_non_weekly_every_token_is_an_error() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "rec-1"
title  = "Bogus recurrence"
start  = "10:00"
end    = "10:30"
every  = "bogus"
anchor = "2026-06-01"
"#;
        let err = Plan::from_toml(toml).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(matches!(
            err,
            PlanError::Validation(ValidationError::BadWeekday { ref id, ref token })
                if id.as_str() == "rec-1" && token == "bogus"
        ));
    }

    // ── invalid approval value in TOML ─────────────────────────────────

    #[test]
    fn invalid_approval_value_fails_toml_parse() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "run-1"
title    = "Runner"
start    = "09:00"
end      = "09:30"
approval = "bogus"
"#;
        assert!(Plan::from_toml(toml).is_err());
    }

    // ── run without approval defaults to Pending ─────────────────────────

    #[test]
    fn run_without_approval_defaults_to_pending() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "run-1"
title = "Automated task"
start = "09:00"
end   = "09:30"
run   = ["/bin/echo", "hello"]
"#;
        let plan = Plan::from_toml(toml).unwrap();
        assert_eq!(plan.blocks[0].approval, Some(Approval::Pending));
    }

    #[test]
    fn run_with_explicit_approval_keeps_it() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "run-1"
title    = "Automated task"
start    = "09:00"
end      = "09:30"
run      = ["/bin/echo", "hello"]
approval = "approved"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        assert_eq!(plan.blocks[0].approval, Some(Approval::Approved));
    }

    #[test]
    fn approval_round_trips_pending_and_approved() {
        // Covers Approval::Pending deserialization and serialization.
        let pending_toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "run-1"
title    = "Runner"
start    = "09:00"
end      = "09:30"
run      = ["/bin/true"]
approval = "pending"
"#;
        let plan = Plan::from_toml(pending_toml).unwrap();
        assert_eq!(plan.blocks[0].approval, Some(Approval::Pending));
        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].approval, plan.blocks[0].approval);

        // Covers Approval::Approved serialization.
        let approved_toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "run-2"
title    = "Runner"
start    = "10:00"
end      = "10:30"
run      = ["/bin/true"]
approval = "approved"
"#;
        let plan = Plan::from_toml(approved_toml).unwrap();
        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].approval, plan.blocks[0].approval);
    }

    #[test]
    fn when_condition_parses_and_displays_variants() {
        assert_eq!(
            "file_exists(/tmp/ready)".parse::<WhenCondition>().unwrap(),
            WhenCondition::FileExists("/tmp/ready".to_owned())
        );
        assert_eq!(
            "file_changed(/tmp/input.txt)"
                .parse::<WhenCondition>()
                .unwrap()
                .to_string(),
            "file_changed(/tmp/input.txt)"
        );
        assert_eq!(
            "command_ok(/bin/true --flag)"
                .parse::<WhenCondition>()
                .unwrap()
                .to_string(),
            "command_ok(/bin/true --flag)"
        );
        assert_eq!(
            "command_ok(/bin/true --flag)"
                .parse::<WhenCondition>()
                .unwrap(),
            WhenCondition::CommandOk(vec!["/bin/true".to_owned(), "--flag".to_owned()])
        );
        assert!("file_exists()".parse::<WhenCondition>().is_err());
        assert!("file_changed()".parse::<WhenCondition>().is_err());
        assert!("command_ok()".parse::<WhenCondition>().is_err());
        assert!("unknown(/tmp/x)".parse::<WhenCondition>().is_err());
    }

    #[test]
    fn when_condition_round_trips_through_toml() {
        let toml = r#"
date = "2026-06-08"
timezone = "UTC"

[[block]]
id = "react"
title = "React"
start = "09:00"
duration = "30m"
when = "file_exists(/tmp/ready)"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        assert_eq!(
            plan.blocks[0].when,
            Some(WhenCondition::FileExists("/tmp/ready".to_owned()))
        );
        let serialized = plan.to_toml().unwrap();
        assert!(serialized.contains("when = \"file_exists(/tmp/ready)\""));
        let plan2 = Plan::from_toml(&serialized).unwrap();
        assert_eq!(plan2.blocks[0].when, plan.blocks[0].when);
    }

    #[test]
    fn bad_when_condition_is_validation_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "UTC"

[[block]]
id = "react"
title = "React"
start = "09:00"
duration = "30m"
when = "file_exists()"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid `when` condition"));
    }

    // ── origin round-trip ────────────────────────────────────────────────

    #[test]
    fn origin_round_trips_through_toml() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "gen-1"
title  = "Generated"
start  = "09:00"
end    = "09:30"
origin = { rule_id = "tpl-1", gen_hash = "abc123" }
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let origin = plan.blocks[0].origin.as_ref().unwrap();
        assert_eq!(origin.rule_id.as_str(), "tpl-1");
        assert_eq!(origin.gen_hash, "abc123");

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].origin, plan.blocks[0].origin);
    }

    // ── retry round-trip ─────────────────────────────────────────────────

    #[test]
    fn retry_round_trips_through_toml() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "job-1"
title = "Job"
start = "09:00"
end   = "09:30"

[block.retry]
count   = 3
backoff = "30s"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let retry = plan.blocks[0].retry.as_ref().unwrap();
        assert_eq!(retry.count, 3);
        assert_eq!(retry.backoff.as_seconds(), 30);

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].retry, plan.blocks[0].retry);
    }

    // ── dependency arrays round-trip ─────────────────────────────────────

    #[test]
    fn dependency_arrays_round_trip_through_toml() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id         = "a"
title      = "A"
start      = "09:00"
end        = "09:30"
after      = ["b", "c"]
on_success = ["d"]
on_failure = ["e"]
on_missed  = ["f"]
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let b = &plan.blocks[0];
        assert_eq!(
            b.after.iter().map(BlockId::as_str).collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(
            b.on_success.iter().map(BlockId::as_str).collect::<Vec<_>>(),
            ["d"]
        );
        assert_eq!(
            b.on_failure.iter().map(BlockId::as_str).collect::<Vec<_>>(),
            ["e"]
        );
        assert_eq!(
            b.on_missed.iter().map(BlockId::as_str).collect::<Vec<_>>(),
            ["f"]
        );

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].after, plan.blocks[0].after);
        assert_eq!(plan2.blocks[0].on_success, plan.blocks[0].on_success);
        assert_eq!(plan2.blocks[0].on_failure, plan.blocks[0].on_failure);
        assert_eq!(plan2.blocks[0].on_missed, plan.blocks[0].on_missed);
    }

    // ── schedule_rev unchanged when approval/origin change ───────────────

    #[test]
    fn schedule_rev_unchanged_for_approval_and_origin() {
        let block = Block {
            id: BlockId::new("focus-1").unwrap(),
            title: "Focus".to_owned(),
            start: "11:00".parse().unwrap(),
            span: Span::End("11:30".parse().unwrap()),
            notify: "0m".parse().unwrap(),
            tags: vec![],
            status: Status::Pending,
            run: None,
            recurrence: None,
            origin: None,
            after: vec![],
            on_success: vec![],
            on_failure: vec![],
            on_missed: vec![],
            retry: None,
            expect_by: None,
            approval: None,
            when: None,
            agent: None,
        };
        let rev = block.schedule_rev();

        let mut with_approval = block.clone();
        with_approval.approval = Some(Approval::Approved);
        assert_eq!(with_approval.schedule_rev(), rev);

        let mut with_when = block.clone();
        with_when.when = Some(WhenCondition::FileExists("/tmp/ready".to_owned()));
        assert_eq!(with_when.schedule_rev(), rev);

        let mut with_origin = block;
        with_origin.origin = Some(Origin {
            rule_id: BlockId::new("tpl").unwrap(),
            gen_hash: "xyz".to_owned(),
        });
        assert_eq!(with_origin.schedule_rev(), rev);
    }

    // ── timezone_name_tests (kept from original) ─────────────────────────

    #[test]
    fn time_zone_lookup_reports_invalid_private_name() {
        let timezone = TimeZoneName("Not/AZone".to_owned());
        assert!(matches!(
            timezone.to_time_zone(),
            Err(FieldParseError::TimeZone { value }) if value == "Not/AZone"
        ));
    }

    #[test]
    fn const_constructors_are_correct() {
        assert_eq!(Lead::from_seconds_const(300).as_seconds(), 300);
        assert_eq!(
            DurationSpec::from_seconds_const(1800).unwrap().as_seconds(),
            1800
        );
        assert!(DurationSpec::from_seconds_const(0).is_none());
        assert!(DurationSpec::from_seconds_const(86401).is_none());
    }

    // ── recurrence round-trips ────────────────────────────────────────────

    #[test]
    fn recurrence_daily_with_count_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "standup"
title  = "Daily standup"
start  = "09:00"
end    = "09:15"
every  = "daily"
anchor = "2026-06-01"
count  = 10
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::Daily);
        assert_eq!(rec.anchor.to_string(), "2026-06-01");
        assert_eq!(rec.end, Some(RecurEnd::Count(10)));

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_daily_with_until_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "standup"
title  = "Daily standup"
start  = "09:00"
end    = "09:15"
every  = "daily"
anchor = "2026-06-01"
until  = "2026-06-30"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::Daily);
        assert!(matches!(&rec.end, Some(RecurEnd::Until(d)) if d.to_string() == "2026-06-30"));

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_daily_no_end_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "standup"
title  = "Daily standup"
start  = "09:00"
end    = "09:15"
every  = "daily"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::Daily);
        assert!(rec.end.is_none());

        let written = plan.to_toml().unwrap();
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_weekday_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "standup"
title  = "Standup"
start  = "09:00"
end    = "09:15"
every  = "weekday"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::Weekday);
        let written = plan.to_toml().unwrap();
        assert!(written.contains("every = \"weekday\""));
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_weekend_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "rest"
title  = "Rest"
start  = "09:00"
end    = "09:15"
every  = "weekend"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::Weekend);
        let written = plan.to_toml().unwrap();
        assert!(written.contains("every = \"weekend\""));
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_weekly_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "meeting"
title  = "Weekly meeting"
start  = "10:00"
end    = "10:30"
every  = "weekly:mon,wed"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert!(matches!(
            &rec.rule,
            RecurRule::Weekly(days) if days.len() == 2
        ));
        let written = plan.to_toml().unwrap();
        assert!(written.contains("weekly:"));
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_every_n_days_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "check"
title  = "Check"
start  = "09:00"
end    = "09:15"
every  = "3d"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::EveryNDays(3));
        let written = plan.to_toml().unwrap();
        assert!(written.contains("every = \"3d\""));
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn recurrence_every_n_weeks_round_trips() {
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "review"
title  = "Biweekly review"
start  = "14:00"
end    = "15:00"
every  = "2w"
anchor = "2026-06-01"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        let rec = plan.blocks[0].recurrence.as_ref().unwrap();
        assert_eq!(rec.rule, RecurRule::EveryNWeeks(2));
        let written = plan.to_toml().unwrap();
        assert!(written.contains("every = \"2w\""));
        let plan2 = Plan::from_toml(&written).unwrap();
        assert_eq!(plan2.blocks[0].recurrence, plan.blocks[0].recurrence);
    }

    #[test]
    fn anchor_without_every_is_silently_ignored() {
        // anchor, count, until without every are silently ignored — no validation error.
        let toml = r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "focus"
title  = "Focus"
start  = "09:00"
end    = "09:30"
anchor = "2026-06-01"
count  = 5
until  = "2026-06-30"
"#;
        let plan = Plan::from_toml(toml).unwrap();
        assert!(plan.blocks[0].recurrence.is_none());
    }

    // ── Deserialize error-path coverage ─────────────────────────────────

    /// Feeds a non-string block ID through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `BlockId::deserialize`.
    #[test]
    fn non_string_block_id_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = 123
title = "X"
start = "09:00"
end   = "09:30"
"#,
        );
        assert!(err.is_err());
    }

    /// Feeds a non-string date through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `PlanDate::deserialize`.
    #[test]
    fn non_string_plan_date_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = 20260608
timezone = "Asia/Kolkata"
"#,
        );
        assert!(err.is_err());
    }

    /// Feeds a non-string timezone through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `TimeZoneName::deserialize`.
    #[test]
    fn non_string_timezone_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = 42
"#,
        );
        assert!(err.is_err());
    }

    /// Feeds a non-string start time through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `ClockTime::deserialize`.
    #[test]
    fn non_string_clock_time_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "focus"
title = "X"
start = 900
end   = "10:00"
"#,
        );
        assert!(err.is_err());
    }

    /// Feeds a non-string duration through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `DurationSpec::deserialize`.
    #[test]
    fn non_string_duration_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "focus"
title    = "X"
start    = "09:00"
duration = 30
"#,
        );
        assert!(err.is_err());
    }

    /// Feeds a non-string notification lead through TOML deserialization to hit the
    /// `String::deserialize(deserializer)?` error branch in `Lead::deserialize`.
    #[test]
    fn non_string_notify_lead_in_toml_triggers_deserialize_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id     = "focus"
title  = "X"
start  = "09:00"
end    = "09:30"
notify = 300
"#,
        );
        assert!(err.is_err());
    }

    /// Passes duplicate block IDs through `from_toml` to hit the
    /// `plan.validate()?` error branch in `from_toml_with_default`.
    #[test]
    fn duplicate_block_id_through_from_toml_hits_validate_error_path() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "focus"
title = "First"
start = "09:00"
end   = "09:30"

[[block]]
id    = "focus"
title = "Duplicate"
start = "10:00"
end   = "10:30"
"#,
        );
        assert!(matches!(
            err.unwrap_err(),
            PlanError::Validation(ValidationError::DuplicateId { .. })
        ));
    }

    #[test]
    fn end_not_after_start_is_a_validation_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "t"
title = "T"
start = "11:00"
end   = "10:59"
"#,
        );
        assert!(matches!(
            err.unwrap_err(),
            PlanError::Validation(ValidationError::EndNotAfterStart { .. })
        ));
    }

    #[test]
    fn end_past_day_is_a_validation_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id       = "t"
title    = "T"
start    = "23:30"
duration = "1h"
"#,
        );
        assert!(matches!(
            err.unwrap_err(),
            PlanError::Validation(ValidationError::EndPastDay { .. })
        ));
    }

    /// Passes a bad retry backoff through `from_toml` to hit the retry parse
    /// error path (lines 439/441 in `Block::from_raw`).
    #[test]
    fn bad_retry_backoff_in_toml_triggers_validation_error() {
        let err = Plan::from_toml(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id    = "job"
title = "Job"
start = "09:00"
end   = "09:30"

[block.retry]
count   = 3
backoff = "bad"
"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn recurring_rules_and_timezone_display_in_unit_binary() {
        // RecurringRules::from_toml/to_toml and TimeZoneName::fmt have zero-count
        // instantiations in the lib unit CGU (only store integration binary calls them),
        // causing llvm-cov report to emit phantom missed lines. Exercise them here.
        let rules = RecurringRules::from_toml(
            "timezone = \"Asia/Kolkata\"\n\
             [[block]]\n\
             id    = \"t\"\ntitle = \"T\"\nstart = \"09:00\"\nend   = \"09:30\"\n",
        )
        .unwrap();
        let out = rules.to_toml().unwrap();
        assert!(out.contains("[[block]]"));
        let tz: TimeZoneName = "Asia/Kolkata".parse().unwrap();
        assert_eq!(tz.to_string(), "Asia/Kolkata");
        // Cover BothEndAndDuration (line 416) and MissingEndOrDuration (line 419) in
        // Block::from_raw — only triggered in integration binary currently.
        let err = Plan::from_toml(
            "date = \"2026-06-08\"\ntimezone = \"Asia/Kolkata\"\n\
             [[block]]\nid = \"t\"\ntitle = \"T\"\nstart = \"09:00\"\n\
             end = \"09:30\"\nduration = \"30m\"\n",
        );
        assert!(matches!(
            err,
            Err(PlanError::Validation(
                ValidationError::BothEndAndDuration { .. }
            ))
        ));
        let err = Plan::from_toml(
            "date = \"2026-06-08\"\ntimezone = \"Asia/Kolkata\"\n\
             [[block]]\nid = \"t\"\ntitle = \"T\"\nstart = \"09:00\"\n",
        );
        assert!(matches!(
            err,
            Err(PlanError::Validation(
                ValidationError::MissingEndOrDuration { .. }
            ))
        ));
    }

    #[test]
    fn validation_error_exit_code_and_schedule_revs_in_unit_binary() {
        // Cover ValidationError::exit_code in the unit test CGU (the rNE85QqoNL integration
        // binary covers it via tests/model.rs, but the unit binary never calls it directly,
        // which causes llvm-cov report to see a phantom zero-count instantiation).
        assert_eq!(
            ValidationError::DuplicateId {
                id: BlockId::new("x").unwrap()
            }
            .exit_code(),
            2
        );
        assert_eq!(
            ValidationError::MissingEndOrDuration {
                id: BlockId::new("x").unwrap()
            }
            .exit_code(),
            2
        );
        // Similarly cover Plan::schedule_revs.
        let plan = Plan {
            date: "2026-06-08".parse().unwrap(),
            timezone: "Asia/Kolkata".parse().unwrap(),
            blocks: vec![],
        };
        assert_eq!(plan.schedule_revs().len(), 0);
    }

    #[test]
    fn status_as_str_covers_all_variants_in_unit_binary() {
        // Status::as_str is called from the integration binary only with Pending/Active
        // (through agenda/now which filter terminal blocks). Cover all arms here so the
        // lib-binary instantiation group has no phantom zero-count lines.
        assert_eq!(Status::Pending.as_str(), "pending");
        assert_eq!(Status::Active.as_str(), "active");
        assert_eq!(Status::Done.as_str(), "done");
        assert_eq!(Status::Skipped.as_str(), "skipped");
        assert_eq!(Status::Missed.as_str(), "missed");
        assert_eq!(Status::Expired.as_str(), "expired");
    }
}
