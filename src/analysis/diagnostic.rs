// SPDX-License-Identifier: MIT

//! What an analysis reports: a [`Diagnostic`] carrying a stable [`DiagnosticCode`], a
//! [`Severity`], the [`Span`] it is about, and a message.
//!
//! Unlike [`crate::sema`], which is fail-fast and stops at the first error, the analysis layer
//! collects every finding in a run. That difference is the point: a tool that reports one defect
//! and exits is of little use for reviewing a program.

use crate::lexer::{LineIndex, Span};

/// How much a finding matters. Everything the analysis layer produces today is a `Warning` —
/// nothing here can stop a program from compiling — but the distinction exists so `sema`'s errors
/// can move onto this type later without reshaping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A class of finding.
///
/// The numeric code is what appears in output and what a future suppression pragma would key on,
/// so **a code is never reused** — retiring a rule leaves its number spent. The `01xx` band is the
/// dataflow checkers; later bands can group by analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    /// A statement that no execution can reach.
    UnreachableCode,
    /// A value written to a binding that nothing reads before it is overwritten or goes out of
    /// scope.
    DeadStore,
    /// A binding that is never read at all.
    UnusedBinding,
}

impl DiagnosticCode {
    /// Every code, so the SARIF emitter can list the rules it might report and a test can hold the
    /// numbering to account.
    pub const ALL: [DiagnosticCode; 3] = [
        DiagnosticCode::UnreachableCode,
        DiagnosticCode::DeadStore,
        DiagnosticCode::UnusedBinding,
    ];

    /// The stable identifier, e.g. `PINP0101`.
    pub fn code(self) -> &'static str {
        match self {
            DiagnosticCode::UnreachableCode => "PINP0101",
            DiagnosticCode::DeadStore => "PINP0102",
            DiagnosticCode::UnusedBinding => "PINP0103",
        }
    }

    /// The rule's short name — kebab-case, the form a suppression pragma would spell.
    pub fn name(self) -> &'static str {
        match self {
            DiagnosticCode::UnreachableCode => "unreachable-code",
            DiagnosticCode::DeadStore => "dead-store",
            DiagnosticCode::UnusedBinding => "unused-binding",
        }
    }

    /// One line describing what the rule looks for, for the SARIF rule listing.
    pub fn description(self) -> &'static str {
        match self {
            DiagnosticCode::UnreachableCode => "Statements that no execution can reach.",
            DiagnosticCode::DeadStore => "Values written to a binding that nothing reads.",
            DiagnosticCode::UnusedBinding => "Bindings that are never read.",
        }
    }

    /// How severe a finding of this class is. Everything is advisory for now.
    pub fn severity(self) -> Severity {
        match self {
            DiagnosticCode::UnreachableCode
            | DiagnosticCode::DeadStore
            | DiagnosticCode::UnusedBinding => Severity::Warning,
        }
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    /// Secondary locations — "first assigned here", "made unreachable by this". Carried and
    /// serialised, but no checker emits them yet.
    pub notes: Vec<(String, Span)>,
}

impl Diagnostic {
    /// A finding at `span`, taking its severity from the rule.
    pub fn new(code: DiagnosticCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: code.severity(),
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// Adds a secondary location.
    pub fn with_note(mut self, message: impl Into<String>, span: Span) -> Diagnostic {
        self.notes.push((message.into(), span));
        self
    }

    /// One line per finding: `line:col: severity[CODE]: message`, then one indented line per note.
    /// A span that was never recorded prints without a position rather than pointing at byte 0.
    pub fn render(&self, line_index: &LineIndex) -> String {
        let mut out = String::new();
        out.push_str(&position_prefix(self.span, line_index));
        out.push_str(self.severity.label());
        out.push('[');
        out.push_str(self.code.code());
        out.push_str("]: ");
        out.push_str(&self.message);
        for (note, span) in &self.notes {
            out.push_str("\n  ");
            out.push_str(&position_prefix(*span, line_index));
            out.push_str("note: ");
            out.push_str(note);
        }
        out
    }
}

/// `line:col: ` for a known span, and nothing at all for an unknown one.
fn position_prefix(span: Span, line_index: &LineIndex) -> String {
    if span.is_unknown() {
        return String::new();
    }
    let (line, column) = line_index.locate(span.start);
    format!("{line}:{column}: ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet;

    #[test]
    fn codes_are_unique_and_stable() {
        // The exact strings are the contract: they appear in output and a suppression pragma would
        // key on them, so a change here is a breaking change, not a rename.
        assert_eq!(DiagnosticCode::UnreachableCode.code(), "PINP0101");
        assert_eq!(DiagnosticCode::DeadStore.code(), "PINP0102");
        assert_eq!(DiagnosticCode::UnusedBinding.code(), "PINP0103");

        let codes: FxHashSet<&str> = DiagnosticCode::ALL.iter().map(|code| code.code()).collect();
        assert_eq!(
            codes.len(),
            DiagnosticCode::ALL.len(),
            "a code is duplicated"
        );
        let names: FxHashSet<&str> = DiagnosticCode::ALL.iter().map(|code| code.name()).collect();
        assert_eq!(
            names.len(),
            DiagnosticCode::ALL.len(),
            "a name is duplicated"
        );
    }

    #[test]
    fn all_lists_every_variant() {
        // The match is exhaustive, so a new variant fails to compile until it is added to `ALL`.
        for code in DiagnosticCode::ALL {
            let listed = match code {
                DiagnosticCode::UnreachableCode
                | DiagnosticCode::DeadStore
                | DiagnosticCode::UnusedBinding => true,
            };
            assert!(listed);
        }
        assert_eq!(DiagnosticCode::ALL.len(), 3);
    }

    #[test]
    fn rendering_places_the_finding() {
        let src = "a = 1\nb = 2\n";
        let line_index = LineIndex::new(src);
        let diagnostic = Diagnostic::new(
            DiagnosticCode::DeadStore,
            Span::new(6, 7), // the `b` on line 2
            "Value assigned to `b` is never read.",
        );
        assert_eq!(
            diagnostic.render(&line_index),
            "2:1: warning[PINP0102]: Value assigned to `b` is never read."
        );
    }

    #[test]
    fn an_unknown_span_renders_without_a_position() {
        let line_index = LineIndex::new("a = 1\n");
        let diagnostic = Diagnostic::new(
            DiagnosticCode::UnusedBinding,
            Span::UNKNOWN,
            "Binding `a` is never read.",
        );
        assert_eq!(
            diagnostic.render(&line_index),
            "warning[PINP0103]: Binding `a` is never read."
        );
    }

    #[test]
    fn notes_render_indented_under_the_finding() {
        let src = "a = 1\na = 2\n";
        let line_index = LineIndex::new(src);
        let diagnostic = Diagnostic::new(
            DiagnosticCode::DeadStore,
            Span::new(0, 1),
            "Value assigned to `a` is never read.",
        )
        .with_note("Overwritten here.", Span::new(6, 7));
        assert_eq!(
            diagnostic.render(&line_index),
            "1:1: warning[PINP0102]: Value assigned to `a` is never read.\n  2:1: note: Overwritten here."
        );
    }

    #[test]
    fn severity_comes_from_the_rule() {
        let diagnostic = Diagnostic::new(DiagnosticCode::DeadStore, Span::UNKNOWN, "x");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(Severity::Error.label(), "error");
        assert_eq!(Severity::Warning.label(), "warning");
    }
}
