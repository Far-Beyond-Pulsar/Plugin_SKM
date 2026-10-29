//! Diagnostics every animation asset validator reports.
//!
//! A diagnostic names where it applies and, when there is a mechanical fix,
//! the repair the editor can offer. Validators only report; they never edit.

use crate::issue::Severity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable machine-readable code, e.g. `graph.pin_type_mismatch`.
    pub code: &'static str,
    pub message: String,
    /// The authored item the diagnostic is about (node id, sample id, ...).
    pub location: Option<String>,
    /// What the user can do about it, when that is known.
    pub repair: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            location: None,
            repair: None,
        }
    }
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }
    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }
    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }
    pub fn repair(mut self, repair: impl Into<String>) -> Self {
        self.repair = Some(repair.into());
        self
    }
}

pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}
