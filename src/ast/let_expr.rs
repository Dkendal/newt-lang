use ::pretty::RcDoc as D;
use serde::Serialize;

use super::*;

use std::collections::HashMap;

#[ast_node]
pub struct LetExpr {
    pub bindings: Bindings,
    pub body: Rc<Ast>,
}

impl LetExpr {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        let mut expr = self.clone();
        expr.bindings = self
            .bindings
            .iter()
            .map(|(k, v)| (k.clone(), f(v)))
            .collect();
        expr.body = f(&self.body).into();
        expr
    }
    /// Replace all identifiers in the body of the let expression with their corresponding
    /// values
    pub fn simplify(&self) -> Ast {
        let mut bindings = self.bindings.clone();
        // simplifiy all bindings first
        for (ident, value) in &self.bindings {
            let new_value = value.simplify();
            bindings.insert(ident.clone(), new_value);
        }

        let (tree, _) = self.body.prewalk(bindings, &|ast, bindings| match ast {
            Ast::Ident(ref id) => {
                let new_value = bindings.get(&id.name).unwrap_or(&ast).clone();
                (new_value, bindings)
            }
            _ => (ast, bindings),
        });

        tree
    }
}
