use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use itertools::Itertools;

use crate::ast::{Assert, Ast, Bindings, Ident, UnionType, UniqueSymbol, UnitTest};

impl Ast {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        match &self {
            Ast::Access(expr) => Ast::Access(expr.map(f)),

            Ast::ApplyGeneric(expr) => Ast::ApplyGeneric(expr.map(f)),

            Ast::Array(node) => Ast::Array(Rc::new(f(node))),

            Ast::Readonly(node) => Ast::Readonly(Rc::new(f(node))),

            Ast::Builtin(expr) => Ast::Builtin(expr.map(f)),

            Ast::FunctionType(expr) => Ast::FunctionType(expr.map(f)),

            Ast::CondExpr(expr) => Ast::CondExpr(expr.map(f)),

            Ast::ExtendsInfixOp(expr) => Ast::ExtendsInfixOp(expr.map(f)),

            Ast::ExtendsExpr(expr) => Ast::ExtendsExpr(expr.map(f)),

            Ast::ExtendsPrefixOp(expr) => Ast::ExtendsPrefixOp(expr.map(f)),

            Ast::IfExpr(expr) => Ast::IfExpr(expr.map(f)),

            Ast::LetExpr(expr) => Ast::LetExpr(expr.map(f)),

            Ast::MappedType(expr) => Ast::MappedType(expr.map(f)),

            Ast::MatchExpr(expr) => Ast::MatchExpr(expr.map(f)),

            Ast::Path(expr) => Ast::Path(expr.map(f)),

            Ast::TypeLiteral(expr) => Ast::TypeLiteral(expr.map(f)),

            Ast::Program(expr) => Ast::Program(expr.map(f)),

            Ast::Statement(node) => Ast::Statement(f(node).into()),

            Ast::Tuple(expr) => Ast::Tuple(expr.map(f)),

            Ast::TypeAlias(expr) => Ast::TypeAlias(expr.map(f)),

            Ast::UnionType(expr) => Ast::UnionType(expr.map(f)),

            Ast::IntersectionType(expr) => Ast::IntersectionType(expr.map(f)),

            _ => self.clone(),
        }
    }

    /// The set of names declared as `unique symbol` anywhere in this tree.
    fn unique_symbols(&self) -> HashSet<String> {
        let acc = RefCell::new(HashSet::new());
        self.postwalk((), &|node, ctx| {
            if let Ast::UniqueSymbolDecl(sym) = &node {
                acc.borrow_mut().insert(sym.name.clone());
            }
            (node, ctx)
        });
        acc.into_inner()
    }

    /// Rewrite every bare `Ident` reference to a declared `unique symbol` into a
    /// `UniqueSymbol` type node (which renders as `typeof <name>`). Unlike the
    /// conditional desugaring in [`Ast::simplify`], this recurses into `unittest`
    /// bodies and `assert` claims — assert claims are otherwise left alone, but a
    /// unique-symbol reference there must still become `typeof <name>` in both
    /// evaluation and generated TypeScript. ComputedPropertyName property keys hold their
    /// `Ident` inside `PropertyName` (not an `Ast::Ident` the walk visits),
    /// so they are intentionally untouched and still render as `[<name>]`.
    fn rewrite_unique_symbols(&self, symbols: &HashSet<String>) -> Ast {
        match self {
            Ast::Ident(Ident { name, span }) if symbols.contains(name) => {
                Ast::UniqueSymbol(UniqueSymbol {
                    name: name.clone(),
                    span: *span,
                })
            }
            Ast::UnitTest(ut) => Ast::UnitTest(UnitTest {
                span: ut.span,
                name: ut.name.clone(),
                body: ut
                    .body
                    .iter()
                    .map(|node| node.rewrite_unique_symbols(symbols))
                    .collect(),
            }),
            Ast::Assert(assert) => Ast::Assert(Assert {
                span: assert.span,
                claim: Rc::new(assert.claim.rewrite_unique_symbols(symbols)),
            }),
            other => other.map(|child| child.rewrite_unique_symbols(symbols)),
        }
    }

    pub fn simplify(&self) -> Self {
        let bindings: Bindings = Default::default();

        let identity = |node, ctx| (node, ctx);

        // First rewrite references to declared `unique symbol`s across the whole
        // program (including `assert` claims, which the desugaring traverse below
        // does not reach), then desugar conditionals in type-alias bodies.
        let symbols = self.unique_symbols();
        let rewritten = if symbols.is_empty() {
            self.clone()
        } else {
            self.rewrite_unique_symbols(&symbols)
        };

        let (tree, _) = rewritten.traverse(bindings, &identity, &|ast, ctx| {
            let span = ast.as_span();

            match ast {
                Ast::IfExpr(if_expr) => (if_expr.simplify(), ctx),
                Ast::MatchExpr(match_expr) => (match_expr.simplify(), ctx),
                Ast::CondExpr(cond_expr) => (cond_expr.simplify(), ctx),
                Ast::LetExpr(let_expr) => (let_expr.simplify(), ctx),
                Ast::Path(path) => (path.simplify(), ctx),
                Ast::UnionType(UnionType { types, .. }) => match types.as_slice() {
                    // Flatten nested union types (both)
                    [Ast::UnionType(UnionType {
                        types: lhs_types, ..
                    }), Ast::UnionType(UnionType {
                        types: rhs_types, ..
                    })] => {
                        let types = lhs_types
                            .clone()
                            .into_iter()
                            .chain(rhs_types.clone())
                            .collect();

                        let ast = Ast::UnionType(UnionType { types, span });

                        (ast, ctx)
                    }
                    // Flatten nested union types (rhs)
                    [lhs, Ast::UnionType(UnionType {
                        types: rhs_types, ..
                    })] => {
                        let mut types = rhs_types.clone();

                        types.push(lhs.clone());

                        let ast = Ast::UnionType(UnionType { types, span });

                        (ast, ctx)
                    }
                    // Flatten nested union types (lhs)
                    [Ast::UnionType(UnionType {
                        types: lhs_types, ..
                    }), rhs] => {
                        let mut types = lhs_types.clone();

                        types.push(rhs.clone());

                        let ast = Ast::UnionType(UnionType { types, span });

                        (ast, ctx)
                    }
                    // Move all intersection to the right
                    types => {
                        let types = types
                            .iter()
                            .sorted_by(|a, b| a.is_intersection().cmp(&b.is_intersection()))
                            .cloned()
                            .collect_vec();

                        let ast = Ast::UnionType(UnionType { types, span });

                        (ast, ctx)
                    }
                },
                _ => (ast, ctx),
            }
        });
        tree
    }

    pub fn prewalk<Context, F>(&self, ctx: Context, pre: &F) -> (Self, Context)
    where
        Context: Clone,
        F: Fn(Self, Context) -> (Self, Context),
    {
        self.traverse(ctx, pre, &|n, c| (n, c))
    }

    pub fn postwalk<Context, F>(&self, ctx: Context, post: &F) -> (Self, Context)
    where
        Context: Clone,
        F: Fn(Self, Context) -> (Self, Context),
    {
        self.traverse(ctx, &|n, c| (n, c), post)
    }

    pub fn traverse<Context, Pre, Post>(
        &self,
        ctx: Context,
        pre: &Pre,
        post: &Post,
    ) -> (Self, Context)
    where
        Context: Clone,
        Pre: Fn(Self, Context) -> (Self, Context),
        Post: Fn(Self, Context) -> (Self, Context),
    {
        let ast = self.clone();

        let (ast, ctx) = pre(ast, ctx);

        let ast = ast.map(|ast| ast.traverse(ctx.clone(), pre, post).0);

        let (ast, acc) = post(ast, ctx);

        (ast, acc)
    }
}
