// SPDX-License-Identifier: MIT

//! Error/Warning reporting layer.

use crate::lexer::{LineIndex, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    // Lower case is expected in diagnostic messages, so we'd have to un-capitalise.
    // Better to have static strings.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A class of diagnostic.
///
/// Known codes for various errors issued by the parser, static analyser, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    /// A statement that no execution path can reach.
    UnreachableCode,
    /// A value written to a binding that nothing reads before it is overwritten or goes out of
    /// scope.
    DeadStore,
    /// A variable that is never used.
    UnusedVariable,
    /// `return` where an expression belongs.
    ReturnAsValue,
    /// A function with a return type whose end can be reached without a value.
    MissingReturn,
    /// `return` in the top-level code of a program.
    ReturnOutsideFunction,
    /// A returned value not assignable to the function's return type.
    ReturnTypeMismatch,
    /// `return` with a value in a `void` function.
    VoidFunctionReturnsValue,
    /// A bare `return` in a function with a return type.
    MissingReturnValue,
    /// A value that can never exist — an `if` every branch of which returns — used as one.
    NeverValue,
}

/// Everything a rule is, in one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleInfo {
    /// The stable identifier, e.g. `PINP1001`.
    pub code: &'static str,
    /// Kebab-case, the form a suppression pragma would spell.
    pub name: &'static str,
    /// One line for the SARIF rule listing.
    pub description: &'static str,
    pub severity: Severity,
}

impl DiagnosticCode {
    /// Every code, so the SARIF emitter can list the rules it might report and a test can hold the
    /// numbering to account.
    pub const ALL: [DiagnosticCode; 10] = [
        DiagnosticCode::UnreachableCode,
        DiagnosticCode::DeadStore,
        DiagnosticCode::UnusedVariable,
        DiagnosticCode::ReturnAsValue,
        DiagnosticCode::MissingReturn,
        DiagnosticCode::ReturnOutsideFunction,
        DiagnosticCode::ReturnTypeMismatch,
        DiagnosticCode::VoidFunctionReturnsValue,
        DiagnosticCode::MissingReturnValue,
        DiagnosticCode::NeverValue,
    ];

    /// The rule's row. The accessors below read from here, so a variant is edited in one place.
    pub const fn info(self) -> RuleInfo {
        match self {
            DiagnosticCode::UnreachableCode => RuleInfo {
                code: "PINP1001",
                name: "unreachable-code",
                description: "Statements that no execution can reach.",
                severity: Severity::Warning,
            },
            DiagnosticCode::DeadStore => RuleInfo {
                code: "PINP1002",
                name: "dead-store",
                description: "Values written to a binding that nothing reads.",
                severity: Severity::Warning,
            },
            DiagnosticCode::UnusedVariable => RuleInfo {
                code: "PINP1003",
                name: "unused-variable",
                description: "Variables that are never used.",
                severity: Severity::Warning,
            },
            DiagnosticCode::ReturnAsValue => RuleInfo {
                code: "PINP2001",
                name: "return-as-value",
                description: "`return` used where a value is expected.",
                severity: Severity::Error,
            },
            DiagnosticCode::MissingReturn => RuleInfo {
                code: "PINP3001",
                name: "missing-return",
                description: "Functions with a return type that can end without a value.",
                severity: Severity::Error,
            },
            DiagnosticCode::ReturnOutsideFunction => RuleInfo {
                code: "PINP3002",
                name: "return-outside-function",
                description: "`return` outside a function.",
                severity: Severity::Error,
            },
            DiagnosticCode::ReturnTypeMismatch => RuleInfo {
                code: "PINP3003",
                name: "return-type-mismatch",
                description: "Returned values of the wrong type.",
                severity: Severity::Error,
            },
            DiagnosticCode::VoidFunctionReturnsValue => RuleInfo {
                code: "PINP3004",
                name: "void-function-returns-value",
                description: "`return` with a value in a void function.",
                severity: Severity::Error,
            },
            DiagnosticCode::MissingReturnValue => RuleInfo {
                code: "PINP3005",
                name: "missing-return-value",
                description: "A bare `return` in a function with a return type.",
                severity: Severity::Error,
            },
            DiagnosticCode::NeverValue => RuleInfo {
                code: "PINP3006",
                name: "never-value",
                description: "Expressions that never produce a value, used as one.",
                severity: Severity::Error,
            },
        }
    }

    /// The stable identifier, e.g. `PINP1001`.
    pub const fn code(self) -> &'static str {
        self.info().code
    }

    /// The rule's short name — kebab-case, the form a suppression pragma would spell.
    pub const fn name(self) -> &'static str {
        self.info().name
    }

    /// One line describing what the rule looks for, for the SARIF rule listing.
    pub const fn description(self) -> &'static str {
        self.info().description
    }

    /// How severe a diagnostic of this class is.
    pub const fn severity(self) -> Severity {
        self.info().severity
    }
}

/// A failure that can be reported: it knows where it happened and what to say.
///
/// Implemented by the lexer's, parser's, and sema's error types so that all three place and render
/// identically. An implementor supplies only [`to_diagnostic`](Self::to_diagnostic); rendering is
/// shared, which is the whole point of the trait — three copies of the same one-liner were what
/// this replaced.
pub trait Diagnose {
    /// This failure as a [`Diagnostic`].
    fn to_diagnostic(&self) -> Diagnostic;

    /// `line:col: error: message`. Takes the [`LineIndex`] for the reason
    /// [`Diagnostic::render`] gives, and is why no `Display` impl exists.
    fn render(&self, line_index: &LineIndex) -> String {
        self.to_diagnostic().render(line_index)
    }
}

/// One diagnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// The rule this diagnostic belongs to. `None` for a lexer, parser, or sema error from before
    /// 0018, which minted the first error codes; giving the older ones codes is a chore of its own.
    pub code: Option<DiagnosticCode>,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    /// Secondary locations — "first assigned here", "made unreachable by this". Carried and
    /// serialised, but no checker emits them yet.
    pub notes: Vec<(String, Span)>,
}

impl Diagnostic {
    /// A rule's diagnostic at `span`, taking its severity from the rule.
    pub fn new(code: DiagnosticCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code: Some(code),
            severity: code.severity(),
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// An error at `span` belonging to no rule — what the lexer, parser, and sema report.
    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code: None,
            severity: Severity::Error,
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

    /// One line per diagnostic: `line:col: severity[CODE]: message`, then one indented line per note.
    /// A diagnostic with no [`DiagnosticCode`] renders `line:col: severity: message` — nothing is
    /// bracketed. A span that was never recorded prints without a position rather than pointing at
    /// byte 0.
    ///
    /// Takes the [`LineIndex`] because a [`Span`] is byte offsets; this is why no `Display` impl
    /// exists, since one could not reach the source to resolve them.
    pub fn render(&self, line_index: &LineIndex) -> String {
        let mut out = String::new();
        out.push_str(&position_prefix(self.span, line_index));
        out.push_str(self.severity.label());
        if let Some(code) = self.code {
            out.push('[');
            out.push_str(code.code());
            out.push(']');
        }
        out.push_str(": ");
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
        assert_eq!(DiagnosticCode::UnreachableCode.code(), "PINP1001");
        assert_eq!(DiagnosticCode::DeadStore.code(), "PINP1002");
        assert_eq!(DiagnosticCode::UnusedVariable.code(), "PINP1003");
        assert_eq!(DiagnosticCode::ReturnAsValue.code(), "PINP2001");
        assert_eq!(DiagnosticCode::MissingReturn.code(), "PINP3001");
        assert_eq!(DiagnosticCode::ReturnOutsideFunction.code(), "PINP3002");
        assert_eq!(DiagnosticCode::ReturnTypeMismatch.code(), "PINP3003");
        assert_eq!(DiagnosticCode::VoidFunctionReturnsValue.code(), "PINP3004");
        assert_eq!(DiagnosticCode::MissingReturnValue.code(), "PINP3005");
        assert_eq!(DiagnosticCode::NeverValue.code(), "PINP3006");

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
                | DiagnosticCode::UnusedVariable
                | DiagnosticCode::ReturnAsValue
                | DiagnosticCode::MissingReturn
                | DiagnosticCode::ReturnOutsideFunction
                | DiagnosticCode::ReturnTypeMismatch
                | DiagnosticCode::VoidFunctionReturnsValue
                | DiagnosticCode::MissingReturnValue
                | DiagnosticCode::NeverValue => true,
            };
            assert!(listed);
        }
        assert_eq!(DiagnosticCode::ALL.len(), 10);
    }

    #[test]
    fn analysis_rules_warn_and_parser_and_sema_rules_are_errors() {
        for code in DiagnosticCode::ALL {
            let expected = match code.code().starts_with("PINP1") {
                true => Severity::Warning,
                false => Severity::Error,
            };
            assert_eq!(code.severity(), expected, "{}", code.code());
        }
    }

    #[test]
    fn rendering_places_the_diagnostic() {
        let src = "a = 1\nb = 2\n";
        let line_index = LineIndex::new(src);
        let diagnostic = Diagnostic::new(
            DiagnosticCode::DeadStore,
            Span::new(6, 7), // the `b` on line 2
            "Value assigned to `b` is never read.",
        );
        assert_eq!(
            diagnostic.render(&line_index),
            "2:1: warning[PINP1002]: Value assigned to `b` is never read."
        );
    }

    #[test]
    fn an_unknown_span_renders_without_a_position() {
        let line_index = LineIndex::new("a = 1\n");
        let diagnostic = Diagnostic::new(
            DiagnosticCode::UnusedVariable,
            Span::UNKNOWN,
            "Variable `a` is never used.",
        );
        assert_eq!(
            diagnostic.render(&line_index),
            "warning[PINP1003]: Variable `a` is never used."
        );
    }

    #[test]
    fn notes_render_indented_under_the_diagnostic() {
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
            "1:1: warning[PINP1002]: Value assigned to `a` is never read.\n  2:1: note: Overwritten here."
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
