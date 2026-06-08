//! macOS launchd scheduler backend.
//!
//! Every function here is process/file/env IO, so each carries a fn-level `coverage(off)`; the pure
//! plist/label/calendar formatting lives in `super::format` (coverage-on, unit-tested on every
//! host). There is intentionally no module-scope `coverage(off)`, so the anti-gaming guard can
//! prove no business logic hides here.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use jiff::tz::TimeZone;

use crate::{
    context::{Scheduler, SchedulerError},
    platform::{
        DoctorCheck, fire_args,
        format::{calendar_interval, launchd_label, xml_escape},
    },
    store::TriggerRecord,
};

#[derive(Debug, Clone)]
pub(crate) struct NativeScheduler {
    binary: PathBuf,
    agents_dir: PathBuf,
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl NativeScheduler {
    pub(crate) fn new() -> Result<Self, SchedulerError> {
        let binary =
            env::current_exe().map_err(|error| SchedulerError::Operation(error.to_string()))?;
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| SchedulerError::Operation("HOME is not set".to_owned()))?;
        Ok(Self {
            binary,
            agents_dir: home.join("Library").join("LaunchAgents"),
        })
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl Scheduler for NativeScheduler {
    fn prepare(&self) -> Result<(), SchedulerError> {
        Ok(())
    }

    fn add(&self, trigger: &TriggerRecord) -> Result<(), SchedulerError> {
        self.remove(&trigger.backend_id)?;
        fs::create_dir_all(&self.agents_dir).map_err(io_error("create LaunchAgents directory"))?;
        let label = launchd_label(&trigger.backend_id);
        let path = plist_path(&self.agents_dir, &label);
        write_plist(&path, &label, &self.binary, trigger)?;
        let output = Command::new("launchctl")
            .args(["bootstrap", &format!("gui/{}", user_id()?)])
            .arg(&path)
            .output()
            .map_err(command_error("launchctl bootstrap"))?;
        ensure_success("launchctl bootstrap", &output)
    }

    fn remove(&self, backend_id: &str) -> Result<(), SchedulerError> {
        let label = launchd_label(backend_id);
        bootout_label(&label)?;
        let path = plist_path(&self.agents_dir, &label);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SchedulerError::Operation(format!(
                "remove `{}` failed: {error}",
                path.display()
            ))),
        }
    }

    fn list(&self) -> Result<Vec<String>, SchedulerError> {
        let mut ids = Vec::new();
        let entries = match fs::read_dir(&self.agents_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
            Err(error) => return Err(SchedulerError::Operation(error.to_string())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| SchedulerError::Operation(error.to_string()))?;
            if let Some(file_name) = entry.file_name().to_str() {
                if let Some(id) = file_name
                    .strip_prefix("io.ccplan.")
                    .and_then(|value| value.strip_suffix(".plist"))
                {
                    ids.push(id.to_owned());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn doctor_check() -> DoctorCheck {
    if command_exists("launchctl").is_err() {
        return DoctorCheck::error(
            "scheduler",
            "`launchctl` is not on PATH",
            "run ccplan from a normal macOS user session",
        );
    }
    match Command::new("launchctl")
        .args(["print", &format!("gui/{}", user_id().unwrap_or_default())])
        .output()
    {
        Ok(output) if output.status.success() => {
            DoctorCheck::ok("scheduler", "launchd GUI session is available")
        }
        Ok(output) => DoctorCheck::error(
            "scheduler",
            output_summary("launchctl print gui/<uid>", &output),
            "log in to the macOS GUI session before running `ccplan apply`",
        ),
        Err(error) => DoctorCheck::error(
            "scheduler",
            error.to_string(),
            "run ccplan from a normal macOS user session",
        ),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn cleanup_after_fire() {
    let Ok(label) = env::var("CCPLAN_LAUNCHD_LABEL") else {
        return;
    };
    let _ = bootout_label(&label);
    if let Ok(path) = env::var("CCPLAN_LAUNCHD_PLIST") {
        let _ = fs::remove_file(path);
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn write_plist(
    path: &Path,
    label: &str,
    binary: &Path,
    trigger: &TriggerRecord,
) -> Result<(), SchedulerError> {
    fs::write(path, launchd_plist_xml(path, label, binary, trigger))
        .map_err(io_error("write LaunchAgent plist"))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn launchd_plist_xml(path: &Path, label: &str, binary: &Path, trigger: &TriggerRecord) -> String {
    let mut arguments = vec![binary.display().to_string()];
    arguments.extend(fire_args(trigger));
    let arguments = arguments
        .iter()
        .map(|argument| format!("      <string>{}</string>", xml_escape(argument)))
        .collect::<Vec<_>>()
        .join("\n");
    let interval = calendar_interval(trigger.scheduled_at, &TimeZone::system());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
{}
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Month</key><integer>{}</integer>
    <key>Day</key><integer>{}</integer>
    <key>Hour</key><integer>{}</integer>
    <key>Minute</key><integer>{}</integer>
    <key>Second</key><integer>{}</integer>
  </dict>
  <key>EnvironmentVariables</key>
  <dict>
    <key>CCPLAN_LAUNCHD_LABEL</key>
    <string>{}</string>
    <key>CCPLAN_LAUNCHD_PLIST</key>
    <string>{}</string>
{}
  </dict>
</dict>
</plist>
"#,
        xml_escape(label),
        arguments,
        interval.month,
        interval.day,
        interval.hour,
        interval.minute,
        interval.second,
        xml_escape(label),
        xml_escape(&path.display().to_string()),
        ccplan_root_plist_entry()
    )
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn ccplan_root_plist_entry() -> String {
    match env::var("CCPLAN_ROOT") {
        Ok(root) if !root.is_empty() => format!(
            "    <key>CCPLAN_ROOT</key>\n    <string>{}</string>",
            xml_escape(&root)
        ),
        Ok(_) | Err(_) => String::new(),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn bootout_label(label: &str) -> Result<(), SchedulerError> {
    let output = Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{}", user_id()?, label)])
        .output()
        .map_err(command_error("launchctl bootout"))?;
    if output.status.success() || is_missing_job(&output) {
        Ok(())
    } else {
        Err(failed_output("launchctl bootout", &output))
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn plist_path(agents_dir: &Path, label: &str) -> PathBuf {
    agents_dir.join(format!("{label}.plist"))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn user_id() -> Result<String, SchedulerError> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(command_error("id -u"))?;
    ensure_success("id -u", &output)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn command_exists(command: &str) -> Result<(), std::io::Error> {
    Command::new(command).arg("help").output().map(|_| ())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn command_error(action: &'static str) -> impl FnOnce(std::io::Error) -> SchedulerError {
    move |error| SchedulerError::Operation(format!("{action} failed: {error}"))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn io_error(action: &'static str) -> impl FnOnce(std::io::Error) -> SchedulerError {
    move |error| SchedulerError::Operation(format!("{action} failed: {error}"))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn ensure_success(action: &str, output: &Output) -> Result<(), SchedulerError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(failed_output(action, output))
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn failed_output(action: &str, output: &Output) -> SchedulerError {
    SchedulerError::Operation(output_summary(action, output))
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn is_missing_job(output: &Output) -> bool {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.contains("No such process")
        || text.contains("No such file")
        || text.contains("Could not find service")
}
