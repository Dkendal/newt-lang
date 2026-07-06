//! Emit TypeScript type-level assertions from `unittest` blocks.
//!
//! When the compiler is run with `--generate-tests`, each `assert <claim>` inside
//! a `unittest` is lowered to a `type` alias whose body wraps the claim in the
//! `Assert<…>` helper. Because the helper only accepts `true`, the emitted
//! TypeScript fails to type-check exactly when the claim does not hold — so the
//! TypeScript compiler itself becomes the test runner.
//!
//! The relational operators map onto a small set of helper types:
//!
//! | claim                | emitted body                  |
//! |----------------------|-------------------------------|
//! | `A <: B`             | `Extends<A, B>`               |
//! | `not (A <: B)`       | `Not<Extends<A, B>>`          |
//! | `A == B` / `A === B` | `Equals<A, B>`                |
//! | `A != B` / `A !== B` | `Not<Equals<A, B>>`           |
//! | `X and Y`            | `And<…X, …Y>`                 |
//! | `X or Y`             | `Or<…X, …Y>`                  |
//!
//! [`expand`] rewrites a program, replacing each `unittest` with the generated
//! aliases (in source position) and collecting the set of helpers referenced.
//! [`render_helpers`] renders only that set — closed over dependencies, since
//! `Not` is defined in terms of `Equals` — inside a `// START/END Newtype Test
//! Helpers` comment fence, which the caller prepends to the emitted file.

use std::collections::BTreeSet;
use std::rc::Rc;

use crate::ast::{
    ApplyGeneric, Ast, ExtendsInfixOp, ExtendsPrefixOp, Ident, InfixOp, Interface, PrefixOp,
    Program, Span, TypeAlias, UnitTest,
};

/// A TypeScript helper type the generated assertions may reference.
///
/// Declaration order is the canonical emission order in the fence (so the set,
/// being a [`BTreeSet`], renders deterministically).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Helper {
    Assert,
    Refute,
    Equals,
    Extends,
    Not,
    And,
    Or,
}

impl Helper {
    /// The TypeScript declaration emitted into the helper fence.
    fn definition(self) -> &'static str {
        match self {
            Helper::Assert => "type Assert<T extends true> = T;",
            Helper::Refute => "type Refute<T extends false> = T;",
            Helper::Equals => {
                "type Equals<A, B> =\n  \
                 (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2)\n  \
                 ? true : false;"
            }
            Helper::Extends => "type Extends<A, B> = [A] extends [B] ? true : false;",
            Helper::Not => "type Not<A> = Equals<A, false>;",
            Helper::And => "type And<A, B> = [A, B] extends [true, true] ? true : false;",
            Helper::Or => {
                "type Or<A, B> = [A] extends [true] ? true : [B] extends [true] ? true : false;"
            }
        }
    }

    /// Helpers this one references in its own body, so the fence stays closed
    /// (e.g. emitting `Not` requires `Equals`).
    fn dependencies(self) -> &'static [Helper] {
        match self {
            Helper::Not => &[Helper::Equals],
            _ => &[],
        }
    }
}

/// The result of [`expand`]: the rewritten program plus the helpers its
/// generated assertions reference.
pub struct Expansion {
    pub ast: Ast,
    pub helpers: BTreeSet<Helper>,
    /// Generated type name -> 1-based source line of the originating `assert`,
    /// in generation (body) order. Consumed by [`build_source_map`].
    pub mappings: Vec<(String, usize)>,
}

/// Rewrite `program`, replacing each `unittest` with the `type` aliases its
/// `assert`s lower to. Non-`unittest` statements are preserved verbatim, in
/// order, so the generated aliases land where the `unittest` was.
///
/// `source` is the original program text, used to compute the source line of
/// each originating `assert` for [`Expansion::mappings`].
pub fn expand(program: &Ast, source: &str) -> Expansion {
    use std::collections::HashMap;

    let mut helpers = BTreeSet::new();
    let mut mappings = Vec::new();
    // Disambiguate unittests whose descriptions slugify to the same identifier so
    // generated type names stay globally unique (duplicate `type` aliases would
    // otherwise be invalid TypeScript). Slugs never contain `__` — `slugify`
    // collapses each run of separators to a single `_` — so the `__N` suffix
    // can't collide with any real slug.
    let mut slug_counts: HashMap<String, usize> = HashMap::new();

    let statements = match program {
        Ast::Program(Program { statements, .. }) => statements.as_slice(),
        other => std::slice::from_ref(other),
    };

    let mut out = Vec::with_capacity(statements.len());
    for statement in statements {
        match as_unittest(statement) {
            Some(unittest) => {
                let base = slugify(&unittest.name);
                let n = {
                    let count = slug_counts.entry(base.clone()).or_insert(0);
                    *count += 1;
                    *count
                };
                let slug = if n == 1 { base } else { format!("{base}__{n}") };
                for alias in generate_aliases(unittest, &slug, source, &mut helpers, &mut mappings)
                {
                    out.push(Ast::Statement(Rc::new(alias)));
                }
            }
            None => out.push(statement.clone()),
        }
    }

    close_dependencies(&mut helpers);

    let ast = Ast::Program(Program {
        statements: out,
        span: program.as_span(),
    });

    Expansion {
        ast,
        helpers,
        mappings,
    }
}

/// Build a Source Map v3 (as JSON) relating the emitted TypeScript back to the
/// originating `.nt` source. `mappings` maps each emitted declaration name to
/// its 1-based source line — both the generated test aliases (from
/// [`Expansion::mappings`]) and ordinary declarations (from
/// [`collect_declaration_mappings`]). `source_name` is the lone `sources` entry
/// and `output_name` (if any) fills the map's `file` field.
///
/// We walk the rendered TypeScript line by line. A line opening a named
/// declaration — `type <name>` or `interface <name>`, optionally `export`-prefixed
/// — whose `<name>` is in `mappings` starts a statement mapped to that source
/// line; every subsequent line (the declaration itself and any wrapped
/// continuation lines) maps to the same source line until a blank line closes the
/// statement (mirroring the old upward comment scan that stopped at a blank line).
/// All positions are 0-based per Source Map v3, so the 1-based source line is
/// emitted as `src - 1`.
pub fn build_source_map(
    rendered_ts: &str,
    mappings: &[(String, usize)],
    source_name: &str,
    output_name: Option<&str>,
) -> String {
    use std::collections::HashMap;

    let lookup: HashMap<&str, usize> = mappings
        .iter()
        .map(|(name, line)| (name.as_str(), *line))
        .collect();

    let mut gen = srcmap_generator::SourceMapGenerator::new(output_name.map(|s| s.to_string()));
    let src_id = gen.add_source(source_name);

    let mut current_src: Option<usize> = None;
    for (gen_line, raw) in rendered_ts.split('\n').enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() {
            // Blank line: statement boundary. Stop mapping.
            current_src = None;
            continue;
        }
        if let Some(name) = declaration_name_of_line(trimmed) {
            if let Some(&src) = lookup.get(name) {
                current_src = Some(src);
            }
        }
        if let Some(src) = current_src {
            gen.add_mapping(gen_line as u32, 0, src_id, (src - 1) as u32, 0);
        }
    }

    gen.to_json()
}

/// The declared name of an emitted `type`/`interface` line (after `trim_start`),
/// or `None` if the line does not open a named declaration. Handles an optional
/// `export ` prefix; the name is the run of identifier characters after the
/// keyword.
fn declaration_name_of_line(trimmed: &str) -> Option<&str> {
    let decl = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let rest = decl
        .strip_prefix("type ")
        .or_else(|| decl.strip_prefix("interface "))?;
    let len = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum();
    Some(&rest[..len])
}

/// Collect `(name, 1-based source line)` for each ordinary top-level declaration
/// (`type` alias or `interface`) in `program`, so [`build_source_map`] can relate
/// the plain compiled output — not just generated test aliases — back to source.
/// `unittest`s, imports, and unnamed statements are skipped.
pub fn collect_declaration_mappings(program: &Ast, source: &str) -> Vec<(String, usize)> {
    let statements = match program {
        Ast::Program(Program { statements, .. }) => statements.as_slice(),
        other => std::slice::from_ref(other),
    };

    statements
        .iter()
        .filter_map(|statement| declaration_name_and_offset(statement))
        .map(|(name, offset)| (name, line_at(source, offset)))
        .collect()
}

/// The declared name and source byte offset of a top-level `type`/`interface`
/// statement, peeling the `Statement` wrapper. `None` for any other node.
fn declaration_name_and_offset(node: &Ast) -> Option<(String, usize)> {
    match node {
        Ast::Statement(inner) => declaration_name_and_offset(inner),
        Ast::TypeAlias(TypeAlias { name, span, .. }) => Some((name.name.clone(), span.start())),
        Ast::Interface(Interface { name, span, .. }) => Some((name.clone(), span.start())),
        _ => None,
    }
}

/// The 1-based source line containing byte `offset`. `offset` must be a char
/// boundary (every span start is), so counting `'\n'` bytes before it is exact.
fn line_at(source: &str, offset: usize) -> usize {
    source[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Render the needed helpers inside the `// START/END Newtype Test Helpers`
/// comment fence. Returns an empty string when no helpers are needed (no tests
/// were generated), so the caller can skip prepending anything.
pub fn render_helpers(helpers: &BTreeSet<Helper>) -> String {
    if helpers.is_empty() {
        return String::new();
    }

    let defs: Vec<&str> = helpers.iter().map(|helper| helper.definition()).collect();

    format!(
        "// START Newtype Test Helpers\n\n{}\n\n// END Newtype Test Helpers\n",
        defs.join("\n\n")
    )
}

/// Generate one `type` alias per `assert` in `unittest`. The alias name is
/// `_newtype_test__<slug>_<index>`, where `<slug>` is the (already
/// disambiguated) test-description slug and `<index>` is the 0-based position of
/// the assert in the block.
fn generate_aliases(
    unittest: &UnitTest,
    slug: &str,
    source: &str,
    helpers: &mut BTreeSet<Helper>,
    mappings: &mut Vec<(String, usize)>,
) -> Vec<Ast> {
    unittest
        .body
        .iter()
        .filter_map(|statement| match statement {
            Ast::Assert(assert) => Some(assert),
            _ => None,
        })
        .enumerate()
        .map(|(index, assert)| {
            let body = wrap_assert(&assert.claim, helpers);

            let name = format!("_newtype_test__{slug}_{index}");
            // 1-based line of the *claim*, not the `assert` keyword: the harness
            // reports failures at the claim position (test_harness::claim_span),
            // so the source-map segment lines newtype's `--> LINE` up with tsgo's
            // error only if it points at the same line.
            let line = line_at(source, assert.claim.as_span().start());
            mappings.push((name.clone(), line));

            Ast::TypeAlias(TypeAlias {
                export: false,
                name: Ident { name, span: span() },
                params: Vec::new(),
                body: Rc::new(body),
                span: span(),
            })
        })
        .collect()
}

/// Wrap a lowered claim in `Assert<…>`.
fn wrap_assert(claim: &Ast, helpers: &mut BTreeSet<Helper>) -> Ast {
    helpers.insert(Helper::Assert);
    apply("Assert", vec![lower(claim, helpers)])
}

/// Lower a relational claim to its TypeScript helper expression, recording the
/// helpers it uses. Non-relational nodes (a bare `true`/`false`, or anything the
/// assert harness wouldn't accept) pass through unchanged.
fn lower(claim: &Ast, helpers: &mut BTreeSet<Helper>) -> Ast {
    match claim {
        Ast::ExtendsPrefixOp(ExtendsPrefixOp {
            op: PrefixOp::Not,
            value,
            ..
        }) => {
            helpers.insert(Helper::Not);
            apply("Not", vec![lower(value, helpers)])
        }

        Ast::ExtendsInfixOp(ExtendsInfixOp { lhs, op, rhs, .. }) => match op {
            InfixOp::Extends => {
                helpers.insert(Helper::Extends);
                apply("Extends", vec![(**lhs).clone(), (**rhs).clone()])
            }
            InfixOp::NotExtends => {
                helpers.insert(Helper::Not);
                helpers.insert(Helper::Extends);
                apply(
                    "Not",
                    vec![apply("Extends", vec![(**lhs).clone(), (**rhs).clone()])],
                )
            }
            InfixOp::Equals | InfixOp::StrictEquals => {
                helpers.insert(Helper::Equals);
                apply("Equals", vec![(**lhs).clone(), (**rhs).clone()])
            }
            InfixOp::NotEquals | InfixOp::StrictNotEquals => {
                helpers.insert(Helper::Not);
                helpers.insert(Helper::Equals);
                apply(
                    "Not",
                    vec![apply("Equals", vec![(**lhs).clone(), (**rhs).clone()])],
                )
            }
            InfixOp::And => {
                helpers.insert(Helper::And);
                apply("And", vec![lower(lhs, helpers), lower(rhs, helpers)])
            }
            InfixOp::Or => {
                helpers.insert(Helper::Or);
                apply("Or", vec![lower(lhs, helpers), lower(rhs, helpers)])
            }
        },

        Ast::TrueKeyword(_) => Ast::TrueKeyword(span()),
        Ast::FalseKeyword(_) => Ast::FalseKeyword(span()),
        other => other.clone(),
    }
}

/// Build `Name<args…>`.
fn apply(name: &str, args: Vec<Ast>) -> Ast {
    Ast::ApplyGeneric(ApplyGeneric {
        receiver: Rc::new(Ast::Ident(Ident {
            name: name.to_string(),
            span: span(),
        })),
        args,
        span: span(),
        from_pipe: false,
    })
}

/// Add the transitive helper dependencies of everything already in `helpers`.
fn close_dependencies(helpers: &mut BTreeSet<Helper>) {
    let mut queue: Vec<Helper> = helpers.iter().copied().collect();
    while let Some(helper) = queue.pop() {
        for &dependency in helper.dependencies() {
            if helpers.insert(dependency) {
                queue.push(dependency);
            }
        }
    }
}

/// Turn a test description into a `snake_case` identifier fragment: lowercased,
/// with each run of non-alphanumeric characters collapsed to a single `_` and
/// leading/trailing underscores trimmed.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut at_separator = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            at_separator = false;
        } else if !at_separator {
            out.push('_');
            at_separator = true;
        }
    }

    out.trim_matches('_').to_string()
}

/// Unwrap a top-level statement to the `unittest` it contains, if any.
fn as_unittest(node: &Ast) -> Option<&UnitTest> {
    match node {
        Ast::UnitTest(unittest) => Some(unittest),
        Ast::Statement(inner) => as_unittest(inner),
        _ => None,
    }
}

/// A placeholder span for synthesized nodes. These nodes are only rendered, not
/// diagnosed, so their span is never consulted.
fn span() -> Span {
    Span::new(0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_newtype_program;
    use crate::typescript::Pretty;

    /// Parse + simplify `src`, expand its tests, and return the full emitted
    /// TypeScript (helper fence + body), matching what the compiler writes.
    fn generate(src: &str) -> String {
        let program = parse_newtype_program(src).unwrap().simplify();
        let expansion = expand(&program, src);
        let header = render_helpers(&expansion.helpers);
        let body = expansion.ast.render_pretty_ts(120);
        if header.is_empty() {
            body
        } else {
            format!("{header}\n{body}")
        }
    }

    #[test]
    fn slugify_collapses_non_alphanumerics() {
        assert_eq!(slugify("a duck is a bird"), "a_duck_is_a_bird");
        assert_eq!(slugify("  Hello, World!  "), "hello_world");
        assert_eq!(slugify("string <: unknown"), "string_unknown");
    }

    #[test]
    fn extends_and_not_emit_minimal_helpers() {
        let src = "interface Duck { fly: () => void, quack: () => void }\n\
            interface Bird { fly: () => void }\n\
            unittest \"a duck is a bird\" do\n\
            \x20 assert Duck <: Bird\n\
            \x20 assert not(Bird <: Duck)\n\
            end";

        let out = generate(src);

        // The minimal helper set: `Not` pulls in `Equals`, but `Refute` is never
        // used by generation and so is omitted.
        assert!(out.contains("// START Newtype Test Helpers"));
        assert!(out.contains("type Assert<T extends true> = T;"));
        assert!(out.contains("type Equals<A, B> ="));
        assert!(out.contains("type Extends<A, B> = [A] extends [B] ? true : false;"));
        assert!(out.contains("type Not<A> = Equals<A, false>;"));
        assert!(!out.contains("Refute"));
        assert!(!out.contains("type And"));
        assert!(!out.contains("type Or"));

        assert!(
            out.contains("type _newtype_test__a_duck_is_a_bird_0 = Assert<Extends<Duck, Bird>>;")
        );
        assert!(out.contains(
            "type _newtype_test__a_duck_is_a_bird_1 = Assert<Not<Extends<Bird, Duck>>>;"
        ));

        // Interfaces are preserved in source order, before the generated tests.
        let duck = out.find("interface Duck").unwrap();
        let test0 = out.find("_newtype_test__a_duck_is_a_bird_0").unwrap();
        assert!(duck < test0);
    }

    #[test]
    fn equality_maps_to_equals_helper() {
        let out = generate("unittest \"eq\" do\n  assert 1 == 1\nend");
        assert!(out.contains("type _newtype_test__eq_0 = Assert<Equals<1, 1>>;"));
        assert!(out.contains("type Equals<A, B> ="));
        assert!(!out.contains("type Extends"));
    }

    #[test]
    fn conjunction_uses_and_helper() {
        let out = generate(
            "unittest \"both\" do\n  assert (string <: unknown) and (number <: unknown)\nend",
        );
        assert!(out.contains(
            "type _newtype_test__both_0 = Assert<And<Extends<string, unknown>, Extends<number, unknown>>>;"
        ));
        assert!(out.contains("type And<A, B>"));
    }

    #[test]
    fn program_without_unittests_emits_no_fence() {
        let out = generate("type Foo as 1");
        assert!(!out.contains("Newtype Test Helpers"));
        assert!(out.contains("type Foo = 1;"));
    }

    #[test]
    fn mapping_line_tracks_claim_not_assert_keyword() {
        // The `assert` keyword is on line 2 but its claim is on line 3. The
        // mapping must point at the claim (line 3) to match the harness `--> 3`,
        // so the conformance runner lines newtype's report up with tsgo's errors.
        let src = "unittest \"t\" do\n  assert\n    number <: string\nend";
        let program = parse_newtype_program(src).unwrap().simplify();
        let expansion = expand(&program, src);
        assert_eq!(
            expansion.mappings,
            vec![("_newtype_test__t_0".to_string(), 3)]
        );
    }

    #[test]
    fn duplicate_unittest_names_get_unique_type_names() {
        // Two unittests slugifying to the same base must not emit colliding
        // identifiers (which would be invalid TypeScript / duplicate decls).
        let src = "unittest \"dup\" do\n  assert 1 <: number\nend\n\
            unittest \"dup\" do\n  assert 2 <: number\nend";
        let program = parse_newtype_program(src).unwrap().simplify();
        let expansion = expand(&program, src);
        let names: Vec<&str> = expansion.mappings.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["_newtype_test__dup_0", "_newtype_test__dup__2_0"]
        );
    }

    #[test]
    fn build_source_map_emits_v3_with_source_and_mappings() {
        // A single generated alias on (rendered) gen line 0 should map back to
        // its assert's 1-based source line.
        let mappings = vec![("_newtype_test__t_0".to_string(), 5usize)];
        let rendered = "type _newtype_test__t_0 = Assert<Extends<string, unknown>>;\n";

        let json = build_source_map(rendered, &mappings, "example.nt", None);

        // A well-formed Source Map v3 with our source registered.
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("example.nt"));
        // `mappings` is present and non-empty (we emitted at least one segment).
        let needle = "\"mappings\":\"";
        let start = json.find(needle).expect("mappings field present") + needle.len();
        let rest = &json[start..];
        let end = rest.find('"').expect("mappings field terminated");
        assert!(!rest[..end].is_empty(), "mappings should be non-empty");
    }

    #[test]
    fn build_source_map_passes_output_name_as_file() {
        let mappings = vec![("_newtype_test__t_0".to_string(), 1usize)];
        let rendered = "type _newtype_test__t_0 = Assert<true>;\n";
        let json = build_source_map(rendered, &mappings, "in.nt", Some("out.ts"));
        assert!(json.contains("out.ts"));
    }

    #[test]
    fn collect_declaration_mappings_covers_type_aliases_and_interfaces() {
        // `type Foo` on line 1, `interface Bird` on line 3, `type Bar` on line 7.
        let src = "type Foo as string\n\
            \n\
            interface Bird {\n  fly: () => void,\n}\n\
            \n\
            type Bar as number";
        let program = parse_newtype_program(src).unwrap().simplify();
        let mappings = collect_declaration_mappings(&program, src);
        assert_eq!(
            mappings,
            vec![
                ("Foo".to_string(), 1),
                ("Bird".to_string(), 3),
                ("Bar".to_string(), 7),
            ]
        );
    }

    #[test]
    fn declaration_name_of_line_handles_export_and_interface() {
        assert_eq!(declaration_name_of_line("type Foo = string;"), Some("Foo"));
        assert_eq!(
            declaration_name_of_line("export type Bar = number;"),
            Some("Bar")
        );
        assert_eq!(declaration_name_of_line("interface Bird {"), Some("Bird"));
        assert_eq!(
            declaration_name_of_line("export interface Duck {"),
            Some("Duck")
        );
        // A wrapped continuation line opens no declaration.
        assert_eq!(declaration_name_of_line("fly: () => void;"), None);
    }
}
