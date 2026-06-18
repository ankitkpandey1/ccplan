//! Cockpit view-model layer (`model`), plus the `ccplan gui` launcher that opens the
//! Tauri desktop app. The pure `model` builders are consumed by the Cockpit app
//! (the `cockpit` crate) and unit-tested here.

pub mod model;

use crate::error::Result;

/// Opens the Cockpit desktop app — the `cockpit` binary built alongside `ccplan`.
///
/// Marked coverage-off: spawning the GUI process is a pure IO boundary. The dispatch
/// arm is exercised by a `cfg(test)` early return, exactly as the old egui launcher was.
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unnecessary_wraps, clippy::needless_return)]
pub(crate) fn launch_cockpit() -> Result<()> {
    #[cfg(test)]
    {
        return Ok(());
    }
    #[cfg(not(test))]
    {
        use crate::error::Error;

        // Integration tests and headless CI set CCPLAN_HEADLESS to exercise this arm
        // without spawning a window.
        if std::env::var_os("CCPLAN_HEADLESS").is_some() {
            return Ok(());
        }

        let exe = std::env::current_exe().map_err(|e| Error::Usage(e.to_string()))?;
        let dir = exe.parent().ok_or_else(|| {
            Error::Usage("cannot locate the ccplan executable directory".to_owned())
        })?;
        let bin = if cfg!(windows) {
            "cockpit.exe"
        } else {
            "cockpit"
        };
        let path = dir.join(bin);
        if !path.exists() {
            return Err(Error::Usage(format!(
                "Cockpit app not found at {}. Build it with `cargo build --release` in \
                 cockpit/src-tauri, or launch the bundled ccplan desktop app directly.",
                path.display()
            )));
        }
        std::process::Command::new(&path)
            .status()
            .map_err(|e| Error::Usage(e.to_string()))?;
        return Ok(());
    }
}
