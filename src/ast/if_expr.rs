use crate::compose;

use super::*;

#[ast_node]
pub struct IfExpr {
    pub condition: Rc<Ast>,
    pub then_branch: Rc<Ast>,
    pub else_branch: Option<Rc<Ast>>,
}

impl IfExpr {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            span: self.span,
            condition: f(&self.condition).into(),
            then_branch: f(&self.then_branch).into(),
            else_branch: self.else_branch.as_ref().map(compose!(f, Into::into)),
        }
    }

    pub fn simplify(&self) -> Ast {
        let else_branch = self
            .else_branch
            .as_ref()
            .map_or_else(|| Ast::NeverKeyword(self.span), |v| (**v).clone());

        expand_to_extends(&self.condition, &self.then_branch, &else_branch)
    }
}

/// Walks a condition the same way [`expand_to_extends`] does and returns the
/// span of the first leaf that is a bare value rather than a comparison — i.e.
/// an arm like `a -> b` (or `if a then …`) where `a` has no relational operator.
/// Returns `None` when every leaf is a comparison, so the condition is
/// well-formed. This is the static-validation counterpart of the `_ => panic!`
/// arms below: it lets [`crate::ast::validate`] report a readable diagnostic
/// before simplification would otherwise panic.
pub(crate) fn malformed_condition_span(condition: &Ast) -> Option<Span> {
    match condition {
        // `not` wraps a nested condition; recurse into it.
        Ast::ExtendsPrefixOp(ExtendsPrefixOp { op, value, .. }) => match op {
            PrefixOp::Not => malformed_condition_span(value),
        },
        // `and`/`or` combine two sub-conditions; the relational operators are
        // the well-formed leaves. Anything else is not a comparison.
        Ast::ExtendsInfixOp(ExtendsInfixOp { lhs, op, rhs, .. }) => match op {
            InfixOp::And | InfixOp::Or => {
                malformed_condition_span(lhs).or_else(|| malformed_condition_span(rhs))
            }
            InfixOp::Extends
            | InfixOp::NotExtends
            | InfixOp::Equals
            | InfixOp::NotEquals
            | InfixOp::StrictEquals
            | InfixOp::StrictNotEquals => None,
        },
        _ => Some(condition.as_span()),
    }
}

/// Expands an if expression into a series of nested ternary expressions
pub(crate) fn expand_to_extends(condition: &Ast, then: &Ast, else_arm: &Ast) -> Ast {
    // Recursive operations
    let out: Option<Ast> = match condition {
        // Unary operators
        Ast::ExtendsPrefixOp(ExtendsPrefixOp { op, value, .. }) => {
            match op {
                // Swap `then` and `else` branches
                PrefixOp::Not if value.is_compatible_with_not_prefix_op() => {
                    Some(expand_to_extends(value, else_arm, then))
                }
                _ => {
                    unreachable!("Expected `not` prefix operator, found {condition:#?}")
                }
            }
        }
        Ast::ExtendsInfixOp(ExtendsInfixOp { lhs, op, rhs, span }) => match op {
            InfixOp::And => {
                let then = expand_to_extends(rhs, then, else_arm);
                Some(expand_to_extends(lhs, &then, else_arm))
            }
            InfixOp::Or => {
                let else_arm = expand_to_extends(rhs, then, else_arm);
                Some(expand_to_extends(lhs, then, &else_arm))
            }
            // DESIGN: equality lowers to mutual assignability; strict == loose for now
            InfixOp::Equals | InfixOp::StrictEquals => {
                // a == b  is  (a extends b) and (b extends a)
                let l = Rc::new(Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: lhs.clone(),
                    op: InfixOp::Extends,
                    rhs: rhs.clone(),
                }));
                let r = Rc::new(Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: rhs.clone(),
                    op: InfixOp::Extends,
                    rhs: lhs.clone(),
                }));
                let cond = Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: l,
                    op: InfixOp::And,
                    rhs: r,
                });
                Some(expand_to_extends(&cond, then, else_arm))
            }
            InfixOp::NotEquals | InfixOp::StrictNotEquals => {
                // a != b  is  (a not-extends b) or (b not-extends a)
                let l = Rc::new(Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: lhs.clone(),
                    op: InfixOp::NotExtends,
                    rhs: rhs.clone(),
                }));
                let r = Rc::new(Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: rhs.clone(),
                    op: InfixOp::NotExtends,
                    rhs: lhs.clone(),
                }));
                let cond = Ast::ExtendsInfixOp(ExtendsInfixOp {
                    span: *span,
                    lhs: l,
                    op: InfixOp::Or,
                    rhs: r,
                });
                Some(expand_to_extends(&cond, then, else_arm))
            }
            _ => None,
        },
        _ => panic!("Expected extends operator, found {condition:#?}"),
    };

    if let Some(v) = out {
        return v;
    }

    // Terminal nodes
    match condition {
        // Binary operators
        Ast::ExtendsInfixOp(ExtendsInfixOp { lhs, op, rhs, .. }) => {
            let span = lhs.as_span().merge(&rhs.as_span());

            // TODO report a syntax error here
            if !lhs.is_typescript_feature() {
                dbg!(lhs);
                unreachable!("value must be desugared before this point");
            }

            if !rhs.is_typescript_feature() {
                dbg!(rhs);
                unreachable!("value must be desugared before this point");
            }

            match op {
                // Equivalent to `lhs extends rhs ? then : else`
                InfixOp::Extends => Ast::from(ExtendsExpr::new(
                    span,
                    lhs.clone(),
                    rhs.clone(),
                    Rc::new(then.clone()),
                    Rc::new(else_arm.clone()),
                )),
                InfixOp::NotExtends => Ast::from(ExtendsExpr::new(
                    span,
                    lhs.clone(),
                    rhs.clone(),
                    Rc::new(else_arm.clone()),
                    Rc::new(then.clone()),
                )),
                _ => unreachable!(),
            }
        }
        _ => panic!("Expected extends operator, found {condition:#?}"),
    }
}
