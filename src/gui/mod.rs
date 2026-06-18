//! Optional Cockpit GUI feature (`--features gui`). No logic here — pure IO boundary.

pub mod model;
mod view;

use crate::{context::ContextRefs, error::Result};

/// Launches the Cockpit GUI window.
///
/// Marked coverage-off: this is the `eframe::run_native` IO boundary. All testable
/// decision logic lives in `model`.
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unnecessary_wraps, clippy::needless_return)]
pub(crate) fn run_gui(_context: &ContextRefs<'_>) -> Result<()> {
    // Unit tests: return immediately.
    #[cfg(test)]
    {
        return Ok(());
    }
    // Integration tests and headless CI: honour CCPLAN_HEADLESS env-var.
    #[cfg(not(test))]
    {
        if std::env::var_os("CCPLAN_HEADLESS").is_some() {
            return Ok(());
        }
        eframe::run_native(
            "ccplan",
            eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_title("ccplan")
                    .with_inner_size([1280.0, 800.0])
                    .with_min_inner_size([960.0, 600.0]),
                ..Default::default()
            },
            Box::new(|_cc| Ok(Box::new(view::CcplanApp::new()))),
        )
        .map_err(|e| crate::error::Error::Usage(e.to_string()))
    }
}
