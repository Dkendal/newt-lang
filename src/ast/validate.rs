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
use crate::validation_error::ValidationError;

use super::if_expr::malformed_condition_span;
use super::*;

impl Ast {
    pub fn validate(&self, source_name: &str, source: &str) -> Vec<Report<'static, ReportSpan>> {
        let reports = RefCell::new(Vec::new());

        let report = |span: Span, error: ValidationError| {
            let start = span.start.min(source.len());
            let end = span.end.min(source.len()).max(start);
            let config = Config::new().with_index_type(IndexType::Byte);
            let range = start..end;
            let label = Label::new((source_name.to_string(), range)).with_message(&error);

            let mut report =
                Report::build(ReportKind::Error, (source_name.to_string(), start..end))
                    .with_config(config)
                    .with_message("Syntax error")
                    .with_label(label)
                    .with_code(&error.to_code());

            if let Some(help) = &error.to_help() {
                report = report.with_help(help);
            }

            reports.borrow_mut().push(report.finish());
        };

        self.postwalk((), &|node, ctx| {
            {
                let node: &Ast = &node;
                match node {
                    Ast::CondExpr(cond) => {
                        for arm in &cond.arms {
                            if let Some(span) = malformed_condition_span(&arm.condition) {
                                report(
                                    span,
                                    ValidationError::ConditionMissingComparison
                                );
                            }
                        }
                    }
                    Ast::IfExpr(if_expr) => {
                        if let Some(span) = malformed_condition_span(&if_expr.condition) {
                            report(span, ValidationError::ConditionMissingComparison);
                        }
                    }

                    Ast::TypeLiteral(type_literal) => {
                        for prop in &type_literal.properties {
                            match &prop.key {
                                PropertyName::ComputedPropertyName(expr) => {
                                    if !expr.is_well_known_symbol() {
                                        if let Some(span) = malformed_condition_span(&expr) {
                                            report(span, ValidationError::ComputedPropertyNameNotWellKnownSymbol);
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
