//! Desktop notification backend.
//!
//! Every function here is environment/process/D-Bus IO, so each carries a fn-level `coverage(off)`
//! (the real notifier is never driven by the in-process test suite, which uses a recording fake).
//! The pure string quoters/escapers it needs on macOS/Windows live in `super::format` (coverage-on,
//! unit-tested on every host). There is intentionally no module-scope `coverage(off)`.

use crate::{
    context::{Notification, Notifier, NotifyError},
    platform::DoctorCheck,
};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeNotifier;

#[cfg_attr(coverage_nightly, coverage(off))]
impl Notifier for NativeNotifier {
    fn check(&self) -> Result<(), NotifyError> {
        check_notification_environment()
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        check_notification_environment()?;
        send_native_notification(notification)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn doctor_check() -> DoctorCheck {
    match check_notification_environment() {
        Ok(()) => DoctorCheck::ok("notifier", "desktop notification environment is present"),
        Err(error) => DoctorCheck::warning(
            "notifier",
            error.to_string(),
            "run `ccplan doctor` inside a graphical desktop session, then rerun `ccplan apply`",
        ),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn check_notification_environment() -> Result<(), NotifyError> {
    platform_notification_check()
}

#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn platform_notification_check() -> Result<(), NotifyError> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return Ok(());
    }
    if runtime_bus_path()
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).exists())
    {
        return Ok(());
    }
    Err(NotifyError::Operation(
        "DBUS_SESSION_BUS_ADDRESS is missing and /run/user/<uid>/bus is unavailable".to_owned(),
    ))
}

#[cfg(target_os = "macos")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn platform_notification_check() -> Result<(), NotifyError> {
    command_available("osascript", "osascript is unavailable")
}

#[cfg(target_os = "windows")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn platform_notification_check() -> Result<(), NotifyError> {
    command_available("powershell.exe", "PowerShell is unavailable")
}

#[cfg(not(any(unix, target_os = "windows")))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn platform_notification_check() -> Result<(), NotifyError> {
    Err(NotifyError::Unavailable)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn command_available(command: &str, message: &str) -> Result<(), NotifyError> {
    std::process::Command::new(command)
        .arg("--help")
        .output()
        .map(|_| ())
        .map_err(|error| NotifyError::Operation(format!("{message}: {error}")))
}

#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn runtime_bus_path() -> Option<String> {
    std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|dir| format!("{dir}/bus"))
}

#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn send_native_notification(notification: &Notification) -> Result<(), NotifyError> {
    notify_rust::Notification::new()
        .summary(&notification.title)
        .body(&notification.body)
        .show()
        .map(|_| ())
        .map_err(|error| NotifyError::Operation(error.to_string()))
}

#[cfg(target_os = "macos")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn send_native_notification(notification: &Notification) -> Result<(), NotifyError> {
    let script = format!(
        "display notification {} with title {}",
        super::format::applescript_string(&notification.body),
        super::format::applescript_string(&notification.title)
    );
    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| NotifyError::Operation(format!("osascript failed: {error}")))?;
    command_success("osascript", &output)
}

#[cfg(target_os = "windows")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn send_native_notification(notification: &Notification) -> Result<(), NotifyError> {
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        super::format::xml_escape(&notification.title),
        super::format::xml_escape(&notification.body)
    );
    let script = format!(
        r#"[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null; $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; $xml.LoadXml({}); $toast = [Windows.UI.Notifications.ToastNotification]::new($xml); [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier("ccplan").Show($toast)"#,
        super::format::powershell_string(&xml)
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|error| NotifyError::Operation(format!("PowerShell toast failed: {error}")))?;
    command_success("PowerShell toast", &output)
}

#[cfg(not(any(unix, target_os = "windows")))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn send_native_notification(_notification: &Notification) -> Result<(), NotifyError> {
    Err(NotifyError::Unavailable)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn command_success(command: &str, output: &std::process::Output) -> Result<(), NotifyError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(NotifyError::Operation(format!(
        "{command} exited with {}: {message}",
        output.status
    )))
}
