#!/usr/bin/env python3
"""Conformance-test runner: cross-check newtype against tsgo (tsc 7.0).

For each `assert` inside a `unittest`, the newtype compiler is BOTH a runner
(it prints an ok/FAILED report to stderr) and a code generator (with
`--generate-tests` it emits a TypeScript file in which every assert becomes a
`type _newtype_test__… = Assert<…>` alias). With `--source-map FILE` the
compiler additionally writes a Source Map v3 JSON file relating each emitted
line back to the originating `.nt` source line -- so attribution no longer
relies on `/** @newtype line:N */` comments embedded in the TypeScript.

This script feeds the same `.nt` source to both oracles and checks that they
AGREE per assertion -- i.e. newtype reports an assertion as passing iff tsgo
type-checks the corresponding alias without error. A "both fail" result is
still agreement; only a PASS/FAIL split is a disagreement.

Algorithm:
  1. Run `newtype --generate-tests --stdin --stdin-filename FILE
     --source-map MAP < FILE`, capturing stdout (the TypeScript), stderr (the
     ok/FAILED report), and reading the emitted Source Map v3 from MAP.
  2. Decode the source map (base64 VLQ) into a gen-line -> src-line table, then
     scan stdout for `type NAME = …` aliases to build the test universe (NAME ->
     source line), recording each statement's emitted gen-line RANGE.
  3. Parse stderr for `--> LINE:COL` pointers -> the set of source lines newtype
     reports as FAILED.
  4. Write the TypeScript to a temp `.ts` file and run `tsgo --noEmit --strict`.
  5. For each tsgo error at TSLINE, find the test whose gen-line range contains
     that line -> that test's source line is a tsgo FAILURE. Errors landing in
     no test's range are unattributed setup problems.
  6. Compare PASS-agreement per test, print a trace, and exit nonzero if any
     test disagrees (or if either oracle could not be run).

Usage:
    python3 scripts/conformance.py [FILE.nt ...]

With no arguments it runs a sensible default set (examples/test.nt and every
`tests/conformance/*.nt` fixture). The newtype binary is built first
(`cargo build`). The `.nt` path is passed as `--stdin-filename` so it appears
as the source map's lone `sources` entry, but the source text is still piped on
stdin.
"""

import argparse
import glob
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(REPO_ROOT, "target", "debug", "newtype")

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
# A generated alias declaration, e.g. `type _newtype_test__foo_0 = …`.
TYPE_RE = re.compile(r"^type\s+(_newtype_test__\w+)\s*=")
# newtype's failure pointer in the stderr report: the ariadne report header,
# e.g. `╭─[ file.nt:35:10 ]`.
NT_FAIL_RE = re.compile(r":(\d+):\d+\s*\]")
# A tsgo diagnostic line, e.g. `/abs/psub.ts:84:49 - error TS2344: …`.
TSGO_ERR_RE = re.compile(r":(\d+):(\d+)\s+-\s+error\s+TS\d+")

# Base64 alphabet used by Source Map v3 VLQ encoding.
_B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
_B64_INDEX = {ch: i for i, ch in enumerate(_B64)}


def strip_ansi(text):
    return ANSI_RE.sub("", text)


def decode_source_map(map_json):
    """Decode a Source Map v3 `mappings` string (base64 VLQ) into a dict
    {gen_line(1-based): src_line(1-based)}. The map has exactly one source, so
    the source-index and source-column fields are ignored; only the cumulative
    source-line field is tracked. Newtype maps every line of a statement to its
    source line, so each gen line carries at most one segment.

    1-based keys/values match newtype's `--> LINE` pointers and tsgo's display."""
    import json as _json

    mappings = _json.loads(map_json)["mappings"]
    result = {}
    src_line = 0  # cumulative, 0-based per the spec
    for gen_line0, group in enumerate(mappings.split(";")):
        if not group:
            continue
        # Take the first segment on this line; decode its VLQ fields.
        segment = group.split(",")[0]
        fields = _decode_vlq(segment)
        if len(fields) < 4:
            continue
        # fields = [gen_col, src_idx_delta, src_line_delta, src_col_delta]
        src_line += fields[2]
        result[gen_line0 + 1] = src_line + 1
    return result


def _decode_vlq(segment):
    """Decode a base64 VLQ segment string into a list of signed integers."""
    values = []
    shift = 0
    acc = 0
    for ch in segment:
        digit = _B64_INDEX[ch]
        continuation = digit & 0x20
        digit &= 0x1F
        acc += digit << shift
        if continuation:
            shift += 5
        else:
            # Lowest bit of the assembled value is the sign.
            value = acc >> 1
            if acc & 1:
                value = -value
            values.append(value)
            shift = 0
            acc = 0
    return values


def find_tsgo():
    """Locate tsgo: PATH first, then `mise which tsgo`, then mise install dirs."""
    found = shutil.which("tsgo")
    if found:
        return found

    try:
        out = subprocess.run(
            ["mise", "which", "tsgo"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        candidate = out.stdout.strip()
        if out.returncode == 0 and candidate and os.path.exists(candidate):
            return candidate
    except (OSError, subprocess.SubprocessError):
        pass

    home = os.path.expanduser("~")
    patterns = [
        os.path.join(home, ".local", "share", "mise", "installs", "node", "*", "bin", "tsgo"),
        os.path.join(home, ".local", "share", "mise", "installs", "*", "*", "bin", "tsgo"),
    ]
    for pattern in patterns:
        matches = sorted(glob.glob(pattern))
        if matches:
            return matches[-1]
    return None


def build_binary():
    print("building newtype (cargo build)…", file=sys.stderr)
    result = subprocess.run(
        ["cargo", "build"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit("cargo build failed")


def run_newtype(source, path):
    """Run newtype with source piped on stdin, `path` used only as the source
    map's `sources` entry, and a temp `.map` file for the Source Map v3 output.

    Returns (stdout_ts, stderr_report, returncode, map_json). A nonzero exit can
    simply mean an assertion failed (which is what we inspect), but with no
    generated output it signals a parse/compile error — the caller distinguishes
    the two. `map_json` is the source map JSON read back from disk (or None if
    the compiler produced no map, e.g. a parse error)."""
    tmp_map = tempfile.NamedTemporaryFile(
        mode="w", suffix=".map", prefix="newtype_conformance_", delete=False
    )
    tmp_map.close()
    try:
        proc = subprocess.run(
            [
                BINARY,
                "--generate-tests",
                "--stdin",
                "--stdin-filename",
                path,
                "--source-map",
                tmp_map.name,
            ],
            input=source,
            capture_output=True,
            text=True,
            cwd=REPO_ROOT,
        )
        with open(tmp_map.name, "r") as f:
            map_json = f.read()
        if not map_json.strip():
            map_json = None
    finally:
        os.unlink(tmp_map.name)
    return proc.stdout, proc.stderr, proc.returncode, map_json


def parse_test_universe(ts_text, srcmap):
    """Walk the emitted TypeScript, recording each `type NAME = …` alias, its
    originating source line (via the source map), and the emitted gen-line RANGE
    [gen_start, gen_end) of its statement — where gen_end is the next blank line
    at/after the declaration (a statement boundary).

    `srcmap` maps 1-based gen line -> 1-based source line. Returns a list of
    dicts {name, src_line, gen_start, gen_end} in emission order."""
    lines = ts_text.split("\n")
    tests = []
    for idx, raw in enumerate(lines):
        t = TYPE_RE.match(raw.strip())
        if not t:
            continue
        gen_start = idx  # 0-based
        # The statement runs until the next blank (trimmed-empty) line.
        gen_end = gen_start + 1
        while gen_end < len(lines) and lines[gen_end].strip() != "":
            gen_end += 1
        src_line = srcmap.get(gen_start + 1)  # srcmap keys are 1-based
        tests.append({
            "name": t.group(1),
            "src_line": src_line,
            "gen_start": gen_start,
            "gen_end": gen_end,
        })
    return tests


def parse_newtype_failures(stderr_report):
    """Source lines newtype flagged FAILED, from `[ FILE:LINE:COL ]` pointers."""
    return {int(m.group(1)) for m in NT_FAIL_RE.finditer(strip_ansi(stderr_report))}


def parse_tsgo_failures(tests, tsgo_stdout):
    """Attribute each tsgo error to the test whose emitted gen-line range
    contains it, yielding that test's source line as a tsgo FAILURE.

    `tests` is the universe from `parse_test_universe` (each with a
    [gen_start, gen_end) range, 0-based); `tsgo_stdout` is tsgo's (ANSI-stripped)
    diagnostic output. Returns (failures, unattributed): a set of source lines,
    and a list of any error lines that fell outside every test's range (e.g. an
    error in a preserved interface or the helper fence) — those are setup
    problems the caller must surface, not assertion failures."""
    failures = set()
    unattributed = []
    for raw in strip_ansi(tsgo_stdout).splitlines():
        m = TSGO_ERR_RE.search(raw)
        if not m:
            continue
        gen_line0 = int(m.group(1)) - 1  # 1-based -> 0-based
        matched = None
        for test in tests:
            if test["gen_start"] <= gen_line0 < test["gen_end"]:
                matched = test
                break
        if matched is not None and matched["src_line"] is not None:
            failures.add(matched["src_line"])
        else:
            unattributed.append(raw.strip())
    return failures, unattributed


def run_file(path, tsgo):
    print("=" * 72)
    print(f"conformance: {path}")
    print("=" * 72)

    with open(path, "r") as f:
        source = f.read()

    ts_stdout, nt_stderr, nt_rc, map_json = run_newtype(source, path)
    srcmap = decode_source_map(map_json) if map_json else {}
    tests = parse_test_universe(ts_stdout, srcmap)
    nt_failures = parse_newtype_failures(nt_stderr)

    if not tests:
        # No generated tests AND a nonzero exit means the compiler itself failed
        # (parse/compile error), not "this file has no unittests" — surface it.
        if nt_rc != 0:
            print(f"  ERROR: newtype exited {nt_rc} with no generated tests "
                  f"(likely a parse/compile error):")
            sys.stderr.write(nt_stderr)
            return False
        print("  (no unittest assertions found)")
        return True

    # Every generated alias must resolve to a source line via the map; a missing
    # mapping means the source map and the emitted TypeScript drifted out of sync.
    unmapped = [t["name"] for t in tests if t["src_line"] is None]
    if unmapped:
        print(f"  ERROR: no source-map entry for generated alias(es) {unmapped}; "
              f"the source map and emitted TypeScript are out of sync.")
        return False

    # Verdicts are keyed by source line, so two asserts sharing one physical line
    # would collapse and could mask a genuine PASS/FAIL split. Require one assert
    # per line rather than silently mis-scoring.
    line_counts = Counter(t["src_line"] for t in tests)
    ambiguous = sorted(line for line, count in line_counts.items() if count > 1)
    if ambiguous:
        print(f"  ERROR: multiple asserts map to source line(s) {ambiguous}; "
              f"put one assert per line so verdicts stay unambiguous.")
        return False

    # Write the emitted TypeScript to a temp .ts file for tsgo (no stdin mode).
    tmp = tempfile.NamedTemporaryFile(
        mode="w", suffix=".ts", prefix="newtype_conformance_", delete=False
    )
    try:
        tmp.write(ts_stdout)
        tmp.close()
        tsgo_proc = subprocess.run(
            [tsgo, "--noEmit", "--strict", tmp.name],
            capture_output=True,
            text=True,
            cwd=REPO_ROOT,
        )
        tsgo_out = tsgo_proc.stdout + "\n" + tsgo_proc.stderr
        tsgo_rc = tsgo_proc.returncode
        tsgo_failures, tsgo_unattributed = parse_tsgo_failures(tests, tsgo_out)
    finally:
        os.unlink(tmp.name)

    # A tsgo error we can't tie to a generated test, or a nonzero exit with no
    # parseable diagnostics at all, is a setup failure — not an assertion result.
    setup_error = False
    if tsgo_unattributed:
        print("  ERROR: tsgo reported error(s) not attributable to any generated test:")
        for err in tsgo_unattributed:
            print(f"      {err}")
        setup_error = True
    if tsgo_rc != 0 and not tsgo_failures and not tsgo_unattributed:
        print(f"  ERROR: tsgo exited {tsgo_rc} but produced no parseable diagnostics.")
        setup_error = True

    all_agree = True
    print(f"  {'SRC':>4}  {'newtype':<8} {'tsgo':<8} {'verdict':<10} assertion")
    print(f"  {'-'*4}  {'-'*8} {'-'*8} {'-'*10} {'-'*40}")
    for test in tests:
        line = test["src_line"]
        nt_pass = line not in nt_failures
        tsgo_pass = line not in tsgo_failures
        agree = nt_pass == tsgo_pass
        all_agree = all_agree and agree
        verdict = "AGREE" if agree else "DISAGREE"
        print(
            f"  {line:>4}  {'PASS' if nt_pass else 'FAIL':<8} "
            f"{'PASS' if tsgo_pass else 'FAIL':<8} {verdict:<10} {test['name']}"
        )

    passed = sum(1 for t in tests if t["src_line"] not in nt_failures)
    status = "ALL AGREE" if all_agree else "DISAGREEMENT DETECTED"
    if setup_error:
        status = "SETUP ERROR"
    print(f"\n  {len(tests)} assertion(s): newtype {passed} pass / "
          f"{len(tests) - passed} fail; {status}")
    return all_agree and not setup_error


def default_files():
    files = []
    example = os.path.join(REPO_ROOT, "examples", "test.nt")
    if os.path.exists(example):
        files.append(example)
    files.extend(sorted(glob.glob(os.path.join(REPO_ROOT, "tests", "conformance", "*.nt"))))
    return files


def main():
    parser = argparse.ArgumentParser(
        description="Cross-check newtype assertions against tsgo (tsc 7.0)."
    )
    parser.add_argument(
        "files",
        nargs="*",
        help="newtype source files to check (default: examples/test.nt and tests/conformance/*.nt)",
    )
    args = parser.parse_args()

    files = args.files if args.files else default_files()
    if not files:
        raise SystemExit("no input files")

    tsgo = find_tsgo()
    if not tsgo:
        raise SystemExit("could not locate tsgo (not on PATH, not under mise)")
    print(f"using tsgo: {tsgo}", file=sys.stderr)

    build_binary()
    if not os.path.exists(BINARY):
        raise SystemExit(f"binary not found after build: {BINARY}")

    all_ok = True
    for path in files:
        if not os.path.exists(path):
            print(f"skip (missing): {path}", file=sys.stderr)
            all_ok = False
            continue
        all_ok = run_file(path, tsgo) and all_ok

    print()
    if all_ok:
        print("RESULT: newtype and tsgo agree on every assertion.")
        sys.exit(0)
    else:
        print("RESULT: disagreement (or setup error) detected.")
        sys.exit(1)


if __name__ == "__main__":
    main()
