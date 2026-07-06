use clap::Parser;
use newtype::test_codegen;
use newtype::test_harness;
use newtype::typescript::Pretty;
use std::io::Read;

#[derive(Debug, Parser)]
#[clap(name = "newtype compiler")]
struct Args {
    #[clap(short, long, value_name = "FILE")]
    input: Option<String>,
    #[clap(short, long, value_name = "FILE")]
    output: Option<String>,
    /// Stop evaluating `unittest` assertions at the first failure.
    #[clap(long)]
    fail_fast: bool,
    /// Treat unresolved type references as errors: render them with error
    /// severity and exit non-zero (evaluation and rendering still run).
    #[clap(long)]
    deny_unresolved: bool,
    /// Mirror TypeScript's `exactOptionalPropertyTypes`: an optional property
    /// `x?: T` no longer accepts `T | undefined` sources in assertions.
    #[clap(long)]
    exact_optional_property_types: bool,
    /// Emit TypeScript type-level assertions for each `unittest` assert, prefixed
    /// with the helper types they need.
    #[clap(long)]
    generate_tests: bool,
    /// Read the source program from stdin. This is the default when neither
    /// `--input` nor `--stdin` is given; the flag makes that choice explicit.
    #[clap(long, conflicts_with = "input")]
    stdin: bool,
    /// The assumed filename for source read from stdin, used as the source map
    /// `sources` entry. Ignored when reading from `--input`.
    #[clap(long, value_name = "PATH")]
    stdin_filename: Option<String>,
    /// Write a Source Map v3 JSON file to this path relating the emitted
    /// TypeScript back to the `.nt` source. Covers ordinary declarations and,
    /// with `--generate-tests`, the generated test aliases too.
    #[clap(long, value_name = "FILE")]
    source_map: Option<String>,
}

fn main() {
    let args = Args::parse();

    // The name recorded as the source map's lone `sources` entry: the input
    // file when reading from disk, the assumed stdin filename when given, else a
    // placeholder. (`--stdin` is the explicit form of the stdin default.)
    let source_name = match (&args.input, &args.stdin_filename) {
        (Some(input_filename), _) => input_filename.clone(),
        (None, Some(name)) => name.clone(),
        (None, None) => "<stdin>".to_string(),
    };

    let input_source = if let Some(input_filename) = &args.input {
        std::fs::read_to_string(input_filename).unwrap()
    } else {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        input
    };

    // Turn internal `panic!`s that dump an AST node into a source-highlighted
    // diagnostic pointing at the offending region of the program.
    newtype::panic_report::install_hook(source_name.clone(), input_source.clone());

    let input = input_source.as_str();

    let result = newtype::parser::parse_newtype_program(input);

    match result {
        Ok(ast) => {
            // Statically validate before simplification: malformed constructs
            // (e.g. an `if`/`cond` condition that is a bare value instead of a
            // comparison) would otherwise panic during `simplify`. Report each
            // diagnostic against the source and exit non-zero.
            let diagnostics = ast.validate(&source_name, input);
            if !diagnostics.is_empty() {
                for diagnostic in &diagnostics {
                    eprintln!(
                        "{}",
                        newtype::report::report_to_string(diagnostic, &source_name, input)
                    );
                }
                std::process::exit(1);
            }

            // Rewrite global sugar aliases (`Array(T)` → `T[]`, …) before the
            // unresolved pass so those spellings never count as references,
            // then report every type reference the file can't resolve.
            // Warnings never block evaluation or rendering; with
            // `--deny-unresolved` they turn the exit code non-zero below.
            let desugared = ast.desugar_globals();

            let unresolved = newtype::ast::unresolved::unresolved_references(&desugared);
            let severity = if args.deny_unresolved {
                newtype::report::Severity::Error
            } else {
                newtype::report::Severity::Warning
            };
            for reference in &unresolved {
                let labels: Vec<_> = reference
                    .spans
                    .iter()
                    .map(|span| (*span, "cannot be resolved to a definition".to_string()))
                    .collect();
                eprintln!(
                    "{}",
                    newtype::report::render_labeled(
                        severity,
                        &source_name,
                        input,
                        &format!("cannot resolve type `{}`", reference.name),
                        &labels,
                        true,
                    )
                );
            }

            // Evaluate and erase `dbg!` calls before simplification: `simplify()`
            // desugars `if`/`cond`/… via `ExtendsExpr::new`, which panics on a
            // `MacroCall` operand, so a `dbg!` inside an `if` branch must be
            // stripped first. Each call prints a Debug report (per pipeline
            // step) to stderr; downstream stages see the program as if `dbg!`
            // weren't there.
            let cleaned = newtype::ast::dbg_expr::expand(
                &desugared,
                input,
                &source_name,
                true,
                &mut std::io::stderr(),
            )
            .expect("writing dbg! reports to stderr failed");

            let simplified = cleaned.simplify();

            // Evaluate `unittest` assertions after simplification but before
            // rendering. Failures are reported to stderr; rendering still
            // proceeds so the emitted TypeScript is always produced.
            let report = test_harness::run(
                &simplified,
                input,
                &source_name,
                test_harness::Config {
                    fail_fast: args.fail_fast,
                    exact_optional_property_types: args.exact_optional_property_types,
                },
                &mut std::io::stderr(),
            )
            .expect("writing the test report to stderr failed");

            // The emitted TypeScript plus the declaration->source-line table the
            // source map is built from. With `--generate-tests` the table also
            // carries the generated test aliases; ordinary declarations are
            // always included so the map covers plain output too.
            let (out, mappings) = if args.generate_tests {
                let expansion = test_codegen::expand(&simplified, input);
                let header = test_codegen::render_helpers(&expansion.helpers);
                let body = expansion.ast.render_pretty_ts(120);
                let out = if header.is_empty() {
                    body
                } else {
                    format!("{header}\n{body}")
                };
                // Preserved statements keep their original spans, so collect their
                // mappings from `simplified` (the expanded AST's generated aliases
                // carry only placeholder spans).
                let mut mappings = expansion.mappings;
                mappings.extend(test_codegen::collect_declaration_mappings(
                    &simplified,
                    input,
                ));
                (out, mappings)
            } else {
                let out = simplified.render_pretty_ts(120);
                let mappings = test_codegen::collect_declaration_mappings(&simplified, input);
                (out, mappings)
            };

            // When `--source-map PATH` is set, emit a Source Map v3 relating the
            // rendered TypeScript back to the `.nt` source. The map's `file` field
            // is the `--output` name if any (purely cosmetic).
            if let Some(path) = &args.source_map {
                let json = test_codegen::build_source_map(
                    &out,
                    &mappings,
                    &source_name,
                    args.output.as_deref(),
                );
                std::fs::write(path, json).unwrap();
            }

            if let Some(ref output_filename) = &args.output {
                std::fs::write(output_filename, out).unwrap();
            } else {
                println!("{}", out);
            }

            // Non-zero exit on any assertion failure — or, with
            // `--deny-unresolved`, on any unresolved reference — after
            // rendering completes.
            if report.has_failures() || (args.deny_unresolved && !unresolved.is_empty()) {
                std::process::exit(1);
            }
        }
        Err(errors) => {
            for error in errors.iter() {
                newtype::report::eprint(&source_name, input, error.span, &error.message);
            }
            std::process::exit(1);
        }
    }
}
