//! Static validation of a *parsed* (pre-simplification) program.
//!
//! Simplification lowers `if`/`cond`/`match` into extends-expressions and, on
//! malformed input, panics deep inside [`crate::ast::if_expr::expand_to_extends`]
//! with an AST dump. This pass runs first and turns those invariants into
//! readable [`ariadne::Report`]s anchored to the offending source span, so the
//! CLI can report an error and exit cleanly instead of panicking.

use std::cell::RefCell;

use ariadne::{Config, IndexType, Label, Report, ReportKind};

use crate::report::ReportSpan;

use super::if_expr::malformed_condition_span;
use super::*;

impl Ast {
    /// Collect an error [`Report`] for every static-validation failure in the
    /// parsed program, in source order, anchored into `source` under
    /// `source_name`. Runs on the AST *before* `simplify`, since
    /// simplification of malformed input panics. Reports are built without
    /// color so their rendering is deterministic and string-comparable.
    pub fn validate(&self, source_name: &str, source: &str) -> Vec<Report<'static, ReportSpan>> {
        let reports = RefCell::new(Vec::new());
        let report = |span: Span, message: &str| {
            // Clamp to the source so a synthesized or stale span cannot make
            // ariadne index out of bounds or see a backwards range.
            let start = span.start.min(source.len());
            let end = span.end.min(source.len()).max(start);
            reports.borrow_mut().push(
                Report::build(ReportKind::Error, (source_name.to_string(), start..end))
                    .with_config(
                        Config::new()
                            .with_index_type(IndexType::Byte)
                            .with_color(false),
                    )
                    .with_message(message)
                    .with_label(
                        Label::new((source_name.to_string(), start..end)).with_message(message),
                    )
                    .finish(),
            );
        };
        // `postwalk` visits every node; the reports accumulate through the
        // shared `RefCell` (its threaded context is per-branch, not cumulative).
        self.postwalk((), &|node, ctx| {
            {
                let node: &Ast = &node;
                match node {
                    Ast::CondExpr(cond) => {
                        for arm in &cond.arms {
                            if let Some(span) = malformed_condition_span(&arm.condition) {
                                report(
                                    span,
                                    "Left hand side of condition branch is missing comparison.",
                                );
                            }
                        }
                    }
                    Ast::IfExpr(if_expr) => {
                        if let Some(span) = malformed_condition_span(&if_expr.condition) {
                            report(span, "Left hand side of condition is missing comparison.");
                        }
                    }

                    Ast::TypeLiteral(type_literal) => {
                        for prop in &type_literal.properties {
                            match &prop.key {
                                PropertyName::ComputedPropertyName(expr) => {
                                    if !expr.is_well_known_symbol() {
                                        if let Some(span) = malformed_condition_span(&expr) {
                                            report(span, "A computed property may only be a well known symbol. In typescript a computed key may be a value expression that is assignable to `any | string | number | symbol`, but newt does not support value expressions.");
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            };
            (node, ctx)
        });
        reports.into_inner()
    }
}
