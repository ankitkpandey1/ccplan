//! Native scheduler and notification backends.

use std::io::Write;

use crate::{context::ContextRefs, error::Result, model::PlanDate, store::TriggerRecord};

mod notify;

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(target_os = "windows")]
mod schtasks;
#[cfg(target_os = "linux")]
mod systemd;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;

#[cfg(target_os = "macos")]
pub(crate) use launchd::NativeScheduler;
pub(crate) use notify::NativeNotifier;
#[cfg(target_os = "windows")]
pub(crate) use schtasks::NativeScheduler;
#[cfg(target_os = "linux")]
pub(crate) use systemd::NativeScheduler;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) use unsupported::NativeScheduler;

pub(crate) fn write_doctor(out: &mut dyn Write, context: &ContextRefs<'_>) -> Result<()> {
    doctor_report(context).write(out)?;
    Ok(())
}

pub(crate) fn cleanup_after_fire() {
    cleanup_after_fire_impl();
}

fn doctor_report(context: &ContextRefs<'_>) -> DoctorReport {
    DoctorReport {
        checks: vec![
            scheduler_doctor_check(),
            notify::doctor_check(),
            timezone_check(context),
        ],
    }
}

#[cfg(target_os = "linux")]
fn scheduler_doctor_check() -> DoctorCheck {
    systemd::doctor_check()
}

#[cfg(target_os = "macos")]
fn scheduler_doctor_check() -> DoctorCheck {
    launchd::doctor_check()
}

#[cfg(target_os = "windows")]
fn scheduler_doctor_check() -> DoctorCheck {
    schtasks::doctor_check()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn scheduler_doctor_check() -> DoctorCheck {
    unsupported::doctor_check()
}

#[cfg(target_os = "macos")]
fn cleanup_after_fire_impl() {
    launchd::cleanup_after_fire();
}

#[cfg(not(target_os = "macos"))]
fn cleanup_after_fire_impl() {}

fn timezone_check(context: &ContextRefs<'_>) -> DoctorCheck {
    let now = context.clock.now();
    let system = now.time_zone().iana_name().unwrap_or("unknown");
    let today = PlanDate::from_jiff_date(now.date());
    match context.store.load_plan(&today) {
        Ok(Some(plan)) => timezone_doctor_check(system, Some(plan.timezone.as_str())),
        Ok(None) => timezone_doctor_check(system, None),
        Err(error) => DoctorCheck::warning(
            "timezone",
            format!("could not read today's plan timezone: {error}"),
            "run `ccplan show` for the affected date and fix plan storage permissions",
        ),
    }
}

fn timezone_doctor_check(system: &str, plan: Option<&str>) -> DoctorCheck {
    match plan {
        Some(plan) if plan != system && system != "unknown" => DoctorCheck::warning(
            "timezone",
            format!("system timezone is {system}; today's plan timezone is {plan}"),
            "run `ccplan set --from <file> --date <date>` with the intended timezone",
        ),
        Some(plan) => DoctorCheck::ok("timezone", format!("system timezone {system}; plan {plan}")),
        None => DoctorCheck::ok(
            "timezone",
            format!("system timezone {system}; no plan for today"),
        ),
    }
}

fn fire_args(trigger: &TriggerRecord) -> Vec<String> {
    vec![
        "fire".to_owned(),
        "--date".to_owned(),
        trigger.date.to_string(),
        "--id".to_owned(),
        trigger.block_id.to_string(),
        "--event".to_owned(),
        trigger.event.to_string(),
        "--rev".to_owned(),
        trigger.rev.to_string(),
        "--at".to_owned(),
        trigger.scheduled_at.to_string(),
    ]
}

#[cfg(test)]
fn trigger_identity(
    date: &PlanDate,
    id_hash: &str,
    rev: &crate::model::ScheduleRev,
    event: crate::lifecycle::Event,
) -> String {
    format!("{date}-{id_hash}-{rev}-{event}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    fn write(&self, out: &mut dyn Write) -> Result<()> {
        for check in &self.checks {
            writeln!(
                out,
                "{}: {} - {}",
                check.component, check.status, check.detail
            )?;
            if let Some(fix) = &check.fix {
                writeln!(out, "fix: {fix}")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorCheck {
    component: &'static str,
    status: DoctorStatus,
    detail: String,
    fix: Option<String>,
}

impl DoctorCheck {
    pub(crate) fn ok(component: &'static str, detail: impl Into<String>) -> Self {
        Self {
            component,
            status: DoctorStatus::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    pub(crate) fn warning(
        component: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            component,
            status: DoctorStatus::Warning,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    pub(crate) fn error(
        component: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            component,
            status: DoctorStatus::Error,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warning,
    Error,
}

impl std::fmt::Display for DoctorStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::{
        DoctorCheck, DoctorReport, fire_args, timezone_check, timezone_doctor_check,
        trigger_identity, write_doctor,
    };
    use crate::{
        context::{Context, RecordingNotifier, RecordingScheduler},
        lifecycle::Event,
        model::{
            Block, BlockId, ClockTime, DurationSpec, Lead, Plan, PlanDate, ScheduleRev, Span,
            Status,
        },
        store::{HistoryPolicy, Store, TriggerRecord},
        time::FixedClock,
    };
    use assert_fs::TempDir;
    use jiff::Zoned;

    #[test]
    fn doctor_report_renders_checks_and_fixes() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck::ok("scheduler", "ready"),
                DoctorCheck::warning("notifier", "missing bus", "log in to a desktop session"),
                DoctorCheck::error("timezone", "bad", "fix it"),
            ],
        };
        let mut output = Vec::new();

        report.write(&mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("scheduler: ok - ready"));
        assert!(output.contains("notifier: warning - missing bus"));
        assert!(output.contains("timezone: error - bad"));
        assert!(output.contains("fix: log in to a desktop session"));
        let mut failing = FailingWriter;
        failing.flush().unwrap();
        assert!(report.write(&mut failing).is_err());
    }

    #[test]
    fn timezone_check_warns_when_plan_differs_from_system() {
        let check = timezone_doctor_check("Asia/Kolkata", Some("Etc/UTC"));

        assert_eq!(
            check,
            DoctorCheck::warning(
                "timezone",
                "system timezone is Asia/Kolkata; today's plan timezone is Etc/UTC",
                "run `ccplan set --from <file> --date <date>` with the intended timezone",
            )
        );
        assert_eq!(
            timezone_doctor_check("Asia/Kolkata", None),
            DoctorCheck::ok(
                "timezone",
                "system timezone Asia/Kolkata; no plan for today"
            )
        );
        assert_eq!(
            timezone_doctor_check("Asia/Kolkata", Some("Asia/Kolkata")),
            DoctorCheck::ok(
                "timezone",
                "system timezone Asia/Kolkata; plan Asia/Kolkata"
            )
        );
    }

    #[test]
    fn timezone_check_reports_plan_load_errors() {
        let (temp, context) = platform_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        std::fs::create_dir_all(context.store.plan_path(&"2026-06-08".parse().unwrap())).unwrap();

        let check = timezone_check(&context.as_refs());

        assert_eq!(check.component, "timezone");
        assert!(
            check
                .detail
                .contains("could not read today's plan timezone")
        );
        drop(temp);
    }

    #[test]
    fn timezone_check_reports_matching_plan_timezone() {
        let (_temp, context) = platform_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        context
            .store
            .set_plan(&single_block_plan(), HistoryPolicy::Preserve)
            .unwrap();

        assert_eq!(
            timezone_check(&context.as_refs()),
            DoctorCheck::ok(
                "timezone",
                "system timezone Asia/Kolkata; plan Asia/Kolkata"
            )
        );
    }

    #[test]
    fn write_doctor_propagates_writer_errors() {
        let (_temp, context) = platform_context_at("2026-06-08T10:00:00+05:30[Asia/Kolkata]");
        let mut failing = FailingWriter;

        assert!(write_doctor(&mut failing, &context.as_refs()).is_err());
    }

    #[test]
    fn fire_arguments_include_absolute_instant_and_revision() {
        let trigger = TriggerRecord {
            backend_id: "2026-06-08-abc-0123456789abcdef-start".to_owned(),
            date: "2026-06-08".parse::<PlanDate>().unwrap(),
            block_id: BlockId::new("focus").unwrap(),
            event: Event::Start,
            rev: ScheduleRev::new("0123456789abcdef").unwrap(),
            scheduled_at: "2026-06-08T05:30:00Z".parse().unwrap(),
        };

        assert_eq!(
            fire_args(&trigger),
            vec![
                "fire",
                "--date",
                "2026-06-08",
                "--id",
                "focus",
                "--event",
                "start",
                "--rev",
                "0123456789abcdef",
                "--at",
                "2026-06-08T05:30:00Z",
            ]
        );
        assert_eq!(
            trigger_identity(&trigger.date, "abcd1234ef", &trigger.rev, trigger.event),
            "2026-06-08-abcd1234ef-0123456789abcdef-start"
        );
    }

    fn platform_context_at(
        now: &str,
    ) -> (
        TempDir,
        Context<FixedClock, RecordingScheduler, RecordingNotifier>,
    ) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path());
        let clock = FixedClock::new(now.parse::<Zoned>().unwrap());
        let context = Context::new(
            store,
            clock,
            RecordingScheduler::default(),
            RecordingNotifier::default(),
        );
        (temp, context)
    }

    fn single_block_plan() -> Plan {
        Plan {
            date: "2026-06-08".parse().unwrap(),
            timezone: "Asia/Kolkata".parse().unwrap(),
            blocks: vec![Block {
                id: BlockId::new("focus").unwrap(),
                title: "Focus".to_owned(),
                start: "11:00".parse::<ClockTime>().unwrap(),
                span: Span::Duration(DurationSpec::from_seconds(1_800).unwrap()),
                notify: Lead::from_seconds(0).unwrap(),
                tags: Vec::new(),
                status: Status::Pending,
                run: None,
            }],
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
