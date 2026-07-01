//! Static validation of a *parsed* (pre-simplification) program.
//!
//! Simplification lowers `if`/`cond`/`match` into extends-expressions and, on
//! malformed input, panics deep inside [`crate::ast::if_expr::expand_to_extends`]
//! with an AST dump. This pass runs first and turns those invariants into
//! readable [`Diagnostic`]s anchored to the offending source span, so the CLI
//! can report an error and exit cleanly instead of panicking.
//!
//! Currently it catches one class: an `if`/`cond` condition that is a bare value
//! instead of a comparison (`a -> b` rather than `a <: b -> …`). New checks can
//! be added by extending [`collect`].

use std::cell::RefCell;

use super::if_expr::malformed_condition_span;
use super::*;

/// A single static-validation error: a message anchored to a source [`Span`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    /// Render this diagnostic against `source` (reported under `source_name`)
    /// as a source-highlighted, underlined report — without color, so the
    /// output is deterministic and string-comparable.
    pub fn to_report_string(&self, source_name: &str, source: &str) -> String {
        crate::report::render_to_string(source_name, source, self.span, &self.message)
    }
}

impl Ast {
    /// Collect every [`Diagnostic`] in the parsed program, in source order.
    /// Runs on the AST *before* `simplify`, since simplification of malformed
    /// input panics.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let diagnostics = RefCell::new(Vec::new());
        // `postwalk` visits every node; the diagnostics accumulate through the
        // shared `RefCell` (its threaded context is per-branch, not cumulative).
        self.postwalk((), &|node, ctx| {
            collect(&node, &mut diagnostics.borrow_mut());
            (node, ctx)
        });
        diagnostics.into_inner()
    }
}

/// Append any diagnostics `node` is directly responsible for. Only `if`/`cond`
/// nodes carry conditions; their descendants are validated when the walk
/// reaches them.
fn collect(node: &Ast, out: &mut Vec<Diagnostic>) {
    match node {
        Ast::CondExpr(cond) => {
            for arm in &cond.arms {
                if let Some(span) = malformed_condition_span(&arm.condition) {
                    out.push(Diagnostic {
                        span,
                        message: "Left hand side of condition branch is missing comparison.".into(),
                    });
                }
            }
        }
        Ast::IfExpr(if_expr) => {
            if let Some(span) = malformed_condition_span(&if_expr.condition) {
                out.push(Diagnostic {
                    span,
                    message: "Left hand side of condition is missing comparison.".into(),
                });
            }
        }
        _ => {}
    }
}
