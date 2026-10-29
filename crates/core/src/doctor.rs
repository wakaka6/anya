//! What Anya needs from the machine, and whether it has it. Each adapter
//! probes its own needs (an input device readable, a permission granted, a
//! session present); the app collects the checks, translates them, prints
//! them, and refuses to start while a required one fails.
//!
//! A check holds codes, never text: the catalog key `doctor.<code>.name`
//! names it, `doctor.<code>.<outcome>` explains the result with `values`
//! as placeholders, and `doctor.<code>.fix` (or the more specific
//! `doctor.<code>.<outcome>.fix`) says what to do when it failed.

/// What a failed check means for starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Anya cannot do its job: startup stops.
    Required,
    /// Something will not work, but the rest does.
    Warning,
}

/// One thing Anya needs and what the probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What is checked, e.g. `input_devices`.
    pub code: &'static str,
    /// What a failure means.
    pub severity: Severity,
    /// Whether the machine has it.
    pub ok: bool,
    /// What the probe found, e.g. `readable` or `not_in_group`.
    pub outcome: &'static str,
    /// Placeholders for the outcome's text, e.g. `("device", "/dev/input/event3")`.
    pub values: Vec<(&'static str, String)>,
}

impl Check {
    /// A check that passed.
    pub fn passed(code: &'static str, severity: Severity, outcome: &'static str) -> Check {
        Check {
            code,
            severity,
            ok: true,
            outcome,
            values: Vec::new(),
        }
    }

    /// A check that failed.
    pub fn failed(code: &'static str, severity: Severity, outcome: &'static str) -> Check {
        Check {
            ok: false,
            ..Check::passed(code, severity, outcome)
        }
    }

    /// Adds a placeholder value.
    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Check {
        self.values.push((name, value.into()));
        self
    }

    /// Whether this check stops Anya from starting.
    pub fn blocks(&self) -> bool {
        !self.ok && self.severity == Severity::Required
    }
}

/// Whether Anya may start: no required check failed.
pub fn healthy(checks: &[Check]) -> bool {
    !checks.iter().any(Check::blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_failed_required_check_blocks_startup() {
        let warn = Check::failed("asr", Severity::Warning, "not_configured");
        let fine = Check::passed("input_devices", Severity::Required, "readable");
        assert!(healthy(&[warn.clone(), fine]));
        let broken = Check::failed("input_devices", Severity::Required, "none");
        assert!(!healthy(&[warn, broken]));
    }
}
