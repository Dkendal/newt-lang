//! Loading of `import` statements.
//!
//! [`load`] reads every module a program imports (transitively), parses and
//! simplifies it, and returns the definitions to merge into the importing
//! program's type environment — so `import { Key } from "./Key"` makes `Key`
//! resolvable in `assert` claims and `where` constraints. Module specifiers
//! resolve relative to the importing file's directory, with an implied `.nt`
//! extension.
//!
//! The merge is flat: a loaded module contributes all of its own top-level
//! definitions (so an exported type's body can reference the module's private
//! helpers) plus its own imports' definitions. On a name collision the
//! importing program's local definition wins. A specifier that names a
//! missing or unexported definition produces a [`ModuleWarning`] and stays
//! unbound, so references to it evaluate as indeterminate.
//!
//! Loading never blocks compilation: every resolution failure (unreadable
//! file, parse error, invalid module, unknown export, namespace import) is a
//! warning and the affected names are simply left unresolved. Name
//! *collisions*, however, are hard errors: a name an import binds must not
//! shadow a local top-level definition nor a name bound by another import.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ast::type_env::top_level_nodes;
use crate::ast::{Ast, ImportClause, Interface, Span, TypeAlias};

/// A non-fatal problem encountered while loading imports, with everything
/// needed to render a source-anchored report: the path and contents of the
/// file the offending `import` appears in (imports of imported modules are
/// attributed to the module, not the root program).
pub struct ModuleWarning {
    pub file: String,
    pub source: String,
    pub span: Span,
    pub message: String,
}

/// The result of loading a program's imports.
pub struct LoadedModules {
    /// Top-level definition nodes (`type`, `interface`, `unique symbol`)
    /// collected from every transitively imported module, already simplified.
    pub definitions: Vec<Ast>,
    pub warnings: Vec<ModuleWarning>,
    /// Hard errors: a name bound by an import (its alias, or the exported
    /// name) collides with a local top-level definition or with another
    /// import's bound name. The run should exit non-zero.
    pub errors: Vec<ModuleWarning>,
}

/// Everything remembered about one loaded module file.
struct Module {
    /// The module's own and transitively imported definitions.
    definitions: Vec<Ast>,
    /// Exported name -> its definition node (for alias renaming).
    exports: HashMap<String, Ast>,
}

/// Load every module `program` imports, resolving specifiers relative to
/// `source_name`'s directory. `source` is the program's text, used to anchor
/// warnings about its own `import` statements.
pub fn load(program: &Ast, source_name: &str, source: &str) -> LoadedModules {
    let mut loader = Loader {
        cache: HashMap::new(),
        loading: HashSet::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let definitions = loader.load_program(program, source_name, source);
    LoadedModules {
        definitions,
        warnings: loader.warnings,
        errors: loader.errors,
    }
}

/// The program `evaluation` should run against: `imported` definitions
/// prepended to `program`'s statements, so local definitions shadow imported
/// ones on a name collision. Rendering should keep using the original
/// `program` — imported definitions are re-emitted by their own modules.
#[must_use]
pub fn augment(program: &Ast, imported: Vec<Ast>) -> Ast {
    if imported.is_empty() {
        return program.clone();
    }
    match program {
        Ast::Program(p) => {
            let mut statements = imported;
            statements.extend(p.statements.iter().cloned());
            Ast::Program(crate::ast::Program {
                statements,
                span: p.span,
            })
        }
        other => {
            let mut statements = imported;
            statements.push(other.clone());
            Ast::Program(crate::ast::Program {
                statements,
                span: other.as_span(),
            })
        }
    }
}

struct Loader {
    cache: HashMap<PathBuf, Rc<Module>>,
    /// Canonical paths currently being loaded, to break import cycles.
    loading: HashSet<PathBuf>,
    warnings: Vec<ModuleWarning>,
    errors: Vec<ModuleWarning>,
}

impl Loader {
    /// Process every `import` statement of `program` (a parsed, simplified
    /// AST), returning the definitions its imports contribute.
    fn load_program(&mut self, program: &Ast, source_name: &str, source: &str) -> Vec<Ast> {
        let base = Path::new(source_name)
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let mut definitions = Vec::new();

        // The names `program` defines at its own top level, and the names
        // already bound by an earlier import — a specifier binding either is
        // a collision error.
        let local_names: HashSet<&str> = top_level_nodes(program)
            .filter_map(definition_name)
            .collect();
        let mut bound_names: HashSet<String> = HashSet::new();

        let imports: Vec<_> = top_level_nodes(program)
            .filter_map(|node| match node {
                Ast::ImportStatement(import) => Some(import.clone()),
                _ => None,
            })
            .collect();

        for import in imports {
            let warn = |message: String| ModuleWarning {
                file: source_name.to_string(),
                source: source.to_string(),
                span: import.span,
                message,
            };

            let specifiers = match &import.import_clause {
                ImportClause::Named(specifiers) => specifiers,
                ImportClause::Namespace { alias } => {
                    self.warnings.push(warn(format!(
                        "namespace imports are not supported yet; `{}` will not resolve",
                        alias.name
                    )));
                    continue;
                }
            };

            let path = match resolve_module_path(&base, &import.module) {
                Ok(path) => path,
                Err(detail) => {
                    self.warnings.push(warn(format!(
                        "cannot resolve module `{}`: {detail}",
                        import.module
                    )));
                    continue;
                }
            };

            if self.loading.contains(&path) {
                // An import cycle: the module's definitions are already being
                // collected further up the stack, so bind nothing here.
                continue;
            }

            let module = match self.load_module(&path) {
                Ok(module) => module,
                Err(detail) => {
                    self.warnings.push(warn(format!(
                        "cannot resolve module `{}`: {detail}",
                        import.module
                    )));
                    continue;
                }
            };

            // A requested name the module does not export stays unbound: its
            // definition (if any) is withheld from the merge so references to
            // it evaluate as indeterminate rather than silently resolving.
            let mut withheld: HashSet<String> = HashSet::new();
            for specifier in specifiers {
                let requested = &specifier.module_export_name.name;
                let Some(def) = module.exports.get(requested) else {
                    withheld.insert(requested.clone());
                    self.warnings.push(warn(format!(
                        "module `{}` does not export a type named `{requested}`",
                        import.module
                    )));
                    continue;
                };
                let local = specifier
                    .alias
                    .as_ref()
                    .unwrap_or(&specifier.module_export_name);
                if local_names.contains(local.name.as_str()) {
                    self.errors.push(warn(format!(
                        "imported name `{0}` collides with a local definition of `{0}`",
                        local.name
                    )));
                } else if !bound_names.insert(local.name.clone()) {
                    self.errors.push(warn(format!(
                        "imported name `{}` is already bound by another import",
                        local.name
                    )));
                }
                if let Some(alias) = &specifier.alias {
                    definitions.push(rename_definition(def, &alias.name));
                }
            }

            definitions.extend(
                module
                    .definitions
                    .iter()
                    .filter(|def| definition_name(def).is_none_or(|name| !withheld.contains(name)))
                    .cloned(),
            );
        }

        definitions
    }

    /// Read, parse, and simplify the module at `path` (canonical), loading its
    /// own imports first. Results are cached per path.
    fn load_module(&mut self, path: &Path) -> Result<Rc<Module>, String> {
        if let Some(module) = self.cache.get(path) {
            return Ok(Rc::clone(module));
        }

        let source = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read `{}` ({err})", path.display()))?;
        let path_name = path.display().to_string();

        let parsed = crate::parser::parse_newtype_program(&source).map_err(|errors| {
            let detail = errors
                .first()
                .map_or_else(|| "unknown parse error".to_string(), |e| e.message.clone());
            format!("cannot parse `{path_name}`: {detail}")
        })?;

        let diagnostics = parsed.validate(&path_name, &source);
        if !diagnostics.is_empty() {
            return Err(format!(
                "`{path_name}` is invalid ({} validation error(s))",
                diagnostics.len()
            ));
        }

        let desugared = parsed.desugar_globals();
        // Strip `dbg!` marks (their watches are meaningless outside the root
        // program) so `simplify` never sees a `MacroCall` operand.
        let (cleaned, _watches) = crate::ast::dbg_expr::expand(&desugared, &source, &path_name);

        self.loading.insert(path.to_path_buf());
        let imported = self.load_program(&cleaned, &path_name, &source);
        self.loading.remove(path);

        let simplified = cleaned.simplify();

        let mut definitions = imported;
        let mut exports = HashMap::new();
        for node in top_level_nodes(&simplified) {
            match node {
                Ast::TypeAlias(alias) => {
                    definitions.push(node.clone());
                    if alias.export {
                        exports.insert(alias.name.name.clone(), node.clone());
                    }
                }
                Ast::Interface(interface) => {
                    definitions.push(node.clone());
                    if interface.export {
                        exports.insert(interface.name.clone(), node.clone());
                    }
                }
                // `unique symbol`s have no export marker; they stay
                // module-internal.
                Ast::UniqueSymbolDecl(_) => definitions.push(node.clone()),
                _ => {}
            }
        }

        let module = Rc::new(Module {
            definitions,
            exports,
        });
        self.cache.insert(path.to_path_buf(), Rc::clone(&module));
        Ok(module)
    }
}

/// Resolve a module specifier against the importing file's directory, adding
/// the `.nt` extension when the specifier has none, and canonicalizing.
fn resolve_module_path(base: &Path, module: &str) -> Result<PathBuf, String> {
    let mut path = base.join(module);
    if path.extension().is_none() {
        path.set_extension("nt");
    }
    path.canonicalize()
        .map_err(|err| format!("cannot read `{}` ({err})", path.display()))
}

/// The top-level name a definition node binds, if it is a definition.
fn definition_name(node: &Ast) -> Option<&str> {
    match node {
        Ast::TypeAlias(alias) => Some(&alias.name.name),
        Ast::Interface(interface) => Some(&interface.name),
        Ast::UniqueSymbolDecl(sym) => Some(&sym.name),
        _ => None,
    }
}

/// A copy of `def` bound under `name` (for `import { Foo as Bar }`).
fn rename_definition(def: &Ast, name: &str) -> Ast {
    match def {
        Ast::TypeAlias(alias) => Ast::TypeAlias(TypeAlias {
            name: crate::ast::Ident {
                name: name.to_string(),
                span: alias.name.span,
            },
            ..alias.clone()
        }),
        Ast::Interface(interface) => Ast::Interface(Interface {
            name: name.to_string(),
            ..interface.clone()
        }),
        other => other.clone(),
    }
}
