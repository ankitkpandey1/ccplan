//! Linux `systemd --user` scheduler backend.

#![cfg_attr(coverage_nightly, coverage(off))]

use std::{
    env,
    path::PathBuf,
    process::{Command, Output},
};

use jiff::{Timestamp, tz::TimeZone};

use crate::{
    context::{Scheduler, SchedulerError},
    platform::{DoctorCheck, fire_args},
    store::TriggerRecord,
};

#[derive(Debug, Clone)]
pub(crate) struct NativeScheduler {
    binary: PathBuf,
}

impl NativeScheduler {
    pub(crate) fn new() -> Result<Self, SchedulerError> {
        let binary =
            env::current_exe().map_err(|error| SchedulerError::Operation(error.to_string()))?;
        if !binary.is_absolute() {
            return Err(SchedulerError::Operation(format!(
                "scheduler target path `{}` is not absolute",
                binary.display()
            )));
        }
        Ok(Self { binary })
    }
}

impl Scheduler for NativeScheduler {
    fn prepare(&self) -> Result<(), SchedulerError> {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "import-environment",
                "DBUS_SESSION_BUS_ADDRESS",
                "DISPLAY",
                "WAYLAND_DISPLAY",
                "XAUTHORITY",
                "CCPLAN_ROOT",
            ])
            .output()
            .map_err(command_error("systemctl --user import-environment"))?;
        ensure_success("systemctl --user import-environment", &output)
    }

    fn add(&self, trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        let calendar = systemd_calendar(trigger.scheduled_at);
        validate_calendar(&calendar)?;

        let unit = unit_name(&trigger.backend_id);
        stop_unit(&unit)?;

        let mut command = Command::new("systemd-run");
        command
            .arg("--user")
            .arg("--collect")
            .arg(format!("--unit={unit}"))
            .arg(format!("--on-calendar={calendar}"))
            .arg("--timer-property=AccuracySec=1s");
        for (name, value) in scheduler_environment() {
            command.arg(format!("--setenv={name}={value}"));
        }
        command.arg(&self.binary);
        command.args(fire_args(trigger));

        let output = command
            .output()
            .map_err(command_error("systemd-run --user"))?;
        ensure_success("systemd-run --user", &output)
    }

    fn remove(&self, backend_id: &str) -> Result<(), SchedulerError> {
        stop_unit(&unit_name(backend_id))
    }

    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "list-timers",
                "ccplan-*",
                "--all",
                "--no-legend",
                "--no-pager",
            ])
            .output()
            .map_err(command_error("systemctl --user list-timers"))?;
        ensure_success("systemctl --user list-timers", &output)?;
        Ok(parse_timer_units(&String::from_utf8_lossy(&output.stdout)))
    }
}

pub(crate) fn doctor_check() -> DoctorCheck {
    for command in ["systemd-run", "systemctl", "systemd-analyze"] {
        if command_exists(command).is_err() {
            return DoctorCheck::error(
                "scheduler",
                format!("required command `{command}` is not on PATH"),
                "install systemd user tools and rerun `ccplan doctor`",
            );
        }
    }

    match Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
    {
        Ok(output) if output.status.success() => DoctorCheck::ok(
            "scheduler",
            format!(
                "systemd user manager is {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ),
        ),
        Ok(output) => DoctorCheck::error(
            "scheduler",
            output_summary("systemctl --user is-system-running", &output),
            "start a graphical login session or enable linger with `loginctl enable-linger $USER`",
        ),
        Err(error) => DoctorCheck::error(
            "scheduler",
            error.to_string(),
            "install systemd user tools and rerun `ccplan doctor`",
        ),
    }
}

fn validate_calendar(calendar: &str) -> Result<(), SchedulerError> {
    let output = Command::new("systemd-analyze")
        .arg("calendar")
        .arg(calendar)
        .output()
        .map_err(command_error("systemd-analyze calendar"))?;
    ensure_success("systemd-analyze calendar", &output)
}

fn stop_unit(unit: &str) -> Result<(), SchedulerError> {
    for suffix in ["timer", "service"] {
        let systemd_unit = format!("{unit}.{suffix}");
        let output = Command::new("systemctl")
            .args(["--user", "stop", &systemd_unit])
            .output()
            .map_err(command_error("systemctl --user stop"))?;
        if !output.status.success() && !is_missing_unit(&output) {
            return Err(failed_output("systemctl --user stop", &output));
        }
    }
    Ok(())
}

fn scheduler_environment() -> Vec<(&'static str, String)> {
    let mut values = Vec::new();
    if let Some(address) = dbus_session_bus_address() {
        values.push(("DBUS_SESSION_BUS_ADDRESS", address));
    }
    for name in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "CCPLAN_ROOT"] {
        if let Ok(value) = env::var(name) {
            if !value.is_empty() {
                values.push((name, value));
            }
        }
    }
    values
}

fn dbus_session_bus_address() -> Option<String> {
    env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env::var("XDG_RUNTIME_DIR")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|dir| format!("unix:path={dir}/bus"))
        })
}

fn systemd_calendar(timestamp: Timestamp) -> String {
    timestamp
        .to_zoned(TimeZone::UTC)
        .strftime("%Y-%m-%d %H:%M:%S UTC")
        .to_string()
}

fn unit_name(backend_id: &str) -> String {
    format!("ccplan-{backend_id}")
}

fn parse_timer_units(output: &str) -> Vec<String> {
    let mut timers = Vec::new();
    for line in output.lines() {
        for field in line.split_whitespace() {
            if let Some(id) = field
                .strip_prefix("ccplan-")
                .and_then(|value| value.strip_suffix(".timer"))
            {
                timers.push(id.to_owned());
            }
        }
    }
    timers.sort();
    timers.dedup();
    timers
}

fn command_exists(command: &str) -> Result<(), std::io::Error> {
    Command::new(command).arg("--version").output().map(|_| ())
}

fn command_error(action: &'static str) -> impl FnOnce(std::io::Error) -> SchedulerError {
    move |error| SchedulerError::Operation(format!("{action} failed: {error}"))
}

fn ensure_success(action: &str, output: &Output) -> Result<(), SchedulerError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(failed_output(action, output))
    }
}

fn failed_output(action: &str, output: &Output) -> SchedulerError {
    SchedulerError::Operation(output_summary(action, output))
}

fn output_summary(action: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    format!("{action} exited with {}: {message}", output.status)
}

fn is_missing_unit(output: &Output) -> bool {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.contains("not loaded") || text.contains("not found") || text.contains("could not be found")
}
