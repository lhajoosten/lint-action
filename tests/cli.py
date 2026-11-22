"""Black-box checks for the packaged lint-gate executable."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
BIN = Path(os.environ["LINT_GATE_BIN"]).resolve()
POLICY = ".lint-action.json"
BASELINE = ".lint-action-baseline.json"


def span(item):
    start = item["range"]["start"]
    end = item["range"]["end"]
    return (start["line"], start["column"]), (end["line"], end["column"])


class CliTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.config()

    def write(self, path, content):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(content, encoding="utf-8")

    def config(self, mode="report-only", limits=None, **options):
        self.write(POLICY, json.dumps({
            "mode": mode,
            "limits": limits if limits is not None else dict(rust=1, typescript=1, tsx=1, python=1),
            **options,
        }))

    def invoke(self, code, command="scan", *arguments):
        result = subprocess.run(
            [BIN, command, "--root", self.root, *arguments], capture_output=True, check=False
        )
        self.assertEqual(result.returncode, code, (result.stdout, result.stderr))
        stream = result.stderr if command == "baseline" and code else result.stdout
        return json.loads(stream), result.stdout

    def duplication(self, mode="report-only", minimum=10, **options):
        self.config(mode, limits={}, duplication={"minimum_tokens": minimum}, **options)

    def test_compare_complexity_scores_and_identities(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            policy = {"mode": "baseline", "limits": {"python": 2}}
            (base / POLICY).write_text(json.dumps(policy))

            def callable_source(name, score):
                return f"def {name}(x):\n" + "    if x: x -= 1\n" * (score - 1) + "    return x\n"

            scores = {"crossed": (2, 3), "worse": (3, 4), "better": (4, 3),
                      "fixed": (3, 2), "steady": (3, 3), "passing": (1, 2)}
            before = {f"{name}.py": callable_source(name, pair[0])
                      for name, pair in scores.items()}
            after = {f"{name}.py": "\n\n" + callable_source(name, pair[1])
                     for name, pair in scores.items()}
            before["deleted.py"] = callable_source("deleted", 3)
            before["old.py"] = after["renamed.py"] = callable_source("moved", 3)
            before["ambiguous.py"] = callable_source("ambiguous", 4)
            after["ambiguous.py"] = callable_source("ambiguous", 3) + callable_source("ambiguous", 1)
            after["fresh.py"] = callable_source("fresh", 3)
            for name, content in before.items():
                (base / name).write_text(content)
            (base / BASELINE).write_bytes(subprocess.check_output([BIN, "baseline", "--root", base]))
            for name, content in reversed(list(after.items())):
                self.write(name, content)

            # Neither the candidate policy nor its own baseline can waive the regression.
            self.config("report-only", {"python": 999})
            self.write(BASELINE, "invalid")
            report, raw = self.invoke(1, "compare", "--base-root", base,
                                      "--base-revision", "base", "--candidate-revision", "candidate")
            delta = report.pop("comparison")
            scan, _ = self.invoke(1, "scan", "--config", base / POLICY)
            self.assertEqual(report, scan)
            self.assertEqual((delta["format_version"], delta["status"], delta["unchanged"]),
                             (1, "complete", 1))
            self.assertEqual((delta["base_revision"], delta["candidate_revision"]), ("base", "candidate"))

            def change_row(change):
                before, after = (change[side] for side in ("previous", "current"))
                return (change["category"], (after or before)["path"], change["match_status"],
                        before and before["score"], after and after["score"])
            self.assertEqual([change_row(change) for change in delta["changes"]], [
                ("new", "crossed.py", "matched", 2, 3),
                ("new", "fresh.py", "unmatched", None, 3),
                ("new", "renamed.py", "unmatched", None, 3),
                ("new", "ambiguous.py", "ambiguous", None, 3),
                ("worsened", "worse.py", "matched", 3, 4),
                ("improved", "better.py", "matched", 4, 3),
                ("fixed", "fixed.py", "matched", 3, 2),
                ("removed", "deleted.py", "unmatched", 3, None),
                ("removed", "old.py", "unmatched", 3, None),
                ("removed", "ambiguous.py", "ambiguous", 4, None),
            ])
            crossing = delta["changes"][0]
            self.assertEqual((crossing["limit"], crossing["previous"]["status"], crossing["current"]["status"]),
                             (2, None, "blocking"))
            self.assertEqual(crossing["current"]["range"]["start"]["line"], 3)
            fixed = delta["changes"][6]
            self.assertEqual((fixed["previous"]["status"], fixed["current"]["status"]), ("baselined", None))

            for name in after:
                (self.root / name).unlink()
            for name, content in after.items():
                self.write(name, content)
            self.assertEqual(self.invoke(1, "compare", "--base-root", base,
                                         "--base-revision", "base", "--candidate-revision", "candidate")[1],
                             raw)

    def test_compare_workspace_duplication(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            (base / POLICY).write_text(json.dumps(
                {"mode": "strict", "limits": {}, "duplication": {"minimum_tokens": 10}}
            ))
            long = "prepare(item); send(item); cleanup(item);"
            short = "send(item); cleanup(item);"

            def source(name, statements):
                return f"function {name}() {{ {statements} }}"

            (base / "a.ts").write_text(source("first", long))
            self.write("a.ts", source("first", long))
            for previous, current, category, shares in (
                (long, short, "improved", (15, 10)),
                (short, long, "worsened", (10, 15)),
            ):
                with self.subTest(category=category):
                    (base / "b.ts").write_text(source("second", previous))
                    self.write("b.ts", source("second", current))
                    report, _ = self.invoke(1, "compare", "--base-root", base)
                    change, = report["comparison"]["changes"]
                    self.assertEqual((change["category"], change["match_status"], change["limit"]),
                                     (category, "matched", 10))
                    for side, score in zip(("previous", "current"), shares):
                        self.assertEqual(change[side]["score"], score)
                        self.assertEqual([item["path"] for item in change[side]["occurrences"]],
                                         ["a.ts", "b.ts"])

            (base / "b.ts").write_text(source("second", long))
            self.write("b.ts", "\n" + source("second", long))
            self.write("c.ts", source("third", long))
            report, _ = self.invoke(1, "compare", "--base-root", base)
            delta = report["comparison"]
            self.assertEqual(delta["unchanged"], 1)
            self.assertEqual([change["category"] for change in delta["changes"]], ["new", "new"])
            self.assertEqual([[part["path"] for part in change["current"]["occurrences"]]
                              for change in delta["changes"]], [["a.ts", "c.ts"], ["b.ts", "c.ts"]])

            (self.root / "c.ts").unlink()
            for filename, name in (("a.ts", "first"), ("b.ts", "second")):
                self.write(filename, source(name, long.replace("item", "other")))
            report, _ = self.invoke(1, "compare", "--base-root", base)
            self.assertEqual([(change["category"], change["match_status"])
                              for change in report["comparison"]["changes"]],
                             [("new", "unmatched"), ("removed", "unmatched")])

    def test_compare_unavailable_preserves_candidate_result(self):
        self.config("strict", {"python": 1})
        self.write("candidate.py", "def f(x): return 1 if x else 0")
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            trusted = ("--config", self.root / POLICY)
            report, _ = self.invoke(1, "compare", *trusted)
            self.assertEqual(report["comparison"]["reason"], "base_root_unavailable")
            self.assertNotIn("unchanged", report["comparison"])
            self.assertEqual(len(report["findings"]), 1)

            for root, kind in ((base / "missing", "invalid_path"), (base, "parse_error")):
                (base / "broken.py").write_text("def broken(:\n")
                report, _ = self.invoke(1, "compare", *trusted, "--base-root", root)
                delta = report["comparison"]
                self.assertEqual((delta["status"], delta["reason"], delta["changes"]),
                                 ("unavailable", "base_analysis_failed", []))
                self.assertEqual(delta["base_diagnostics"][0]["kind"], kind)
                self.assertEqual(report["diagnostics"], [])

            self.write("candidate.py", "def broken(:\n")
            (base / "broken.py").write_text("def passing(): pass")
            report, _ = self.invoke(2, "compare", *trusted, "--base-root", base)
            self.assertEqual(report["comparison"]["reason"], "candidate_analysis_failed")
            self.assertEqual(report["diagnostics"][0]["kind"], "parse_error")

    def test_source_duplication_locations_selection_and_threshold(self):
        self.duplication(exclude=["vendor/**"])
        self.write("old.ts", "function old() {\n  const café = 0; prepare(item); send(item);\n}\n")
        self.write("src/new.tsx", """function copy() {
  prepare( item ); // comments do not split a match
  send(item);
}
function another() { prepare(item); send(item); }
""")
        self.write("vendor/ignored.ts", "function excluded() { prepare(item); send(item); }")
        self.write("different.ts", 'function different() { prepare(other); send("item"); }')
        self.write("ignored.py", "def copy(): pass")
        report, raw = self.invoke(0)
        self.assertEqual(report["files_scanned"], 3)
        self.assertEqual(report["diagnostics"], [])
        self.assertEqual(len(report["findings"]), 3)
        self.assertTrue(all(
            (item["rule"], item["score"], item["limit"], item["status"])
            == ("source-duplication", 10, 10, "reported")
            for item in report["findings"]
        ))
        self.assertEqual(report["findings"][0]["occurrences"], [
            {"path": "old.ts", "range": {"start": {"line": 2, "column": 20},
                                        "end": {"line": 2, "column": 46}}},
            {"path": "src/new.tsx", "range": {"start": {"line": 2, "column": 3},
                                              "end": {"line": 3, "column": 14}}},
        ])
        self.assertEqual([item["path"] for item in report["findings"][-1]["occurrences"]],
                         ["src/new.tsx", "src/new.tsx"])
        self.assertEqual(self.invoke(0)[1], raw)
        selected, _ = self.invoke(0, "scan", "--", "src/new.tsx", "vendor/ignored.ts")
        self.assertEqual(len(selected["findings"]), 1)
        self.duplication("strict", 11, exclude=["vendor/**"])
        self.assertEqual(self.invoke(0)[0]["findings"], [])
        self.duplication("strict", 10, exclude=["vendor/**"])
        self.invoke(1)
        self.config("strict", limits={"typescript": 100, "tsx": 100}, exclude=["vendor/**"])
        self.assertEqual(self.invoke(0)[0]["findings"], [])

    def test_source_duplication_keeps_token_spellings(self):
        expression = 'record(item + "value");'
        self.write("first.ts", f"function first() {{ {expression} }}")
        self.write("second.tsx", f"function second() {{ {expression} }}")
        self.duplication(minimum=9)
        self.assertEqual(self.invoke(0)[0]["findings"][0]["score"], 9)
        for old, new in (("item", "other"), ("+", "-"), ('"value"', '"changed"')):
            self.write("second.tsx", f"function second() {{ {expression.replace(old, new)} }}")
            self.assertEqual(self.invoke(0)[0]["findings"], [])
        self.duplication(minimum=2**32 - 1)
        self.assertEqual(self.invoke(0)[0]["findings"], [])

    def test_source_duplication_preserves_whitespace_inside_jsx_text(self):
        # The body has 13 tokens: record ( ) ; return < p > [Hello world] </ p > ;
        # At that minimum, a shorter run on either side of the JSX text cannot qualify.
        self.duplication(minimum=13)
        original = """function original() {
  record();
  return <p>Hello world</p>;
}
"""
        control = original.replace("function original", "function copy")
        formatted = """function copy() {
    record ( );
    return <p>Hello world</p> ;
}
"""
        self.write("a.tsx", original)
        original_occurrence = ("a.tsx", ((2, 3), (3, 29)))
        cases = [
            ("matching control", control, [original_occurrence, ("b.tsx", ((2, 3), (3, 29)))]),
            ("inter-token whitespace", formatted, [
                original_occurrence, ("b.tsx", ((2, 5), (3, 32))),
            ]),
            ("JSX text whitespace", formatted.replace("Hello world", "Hello  world"), None),
        ]
        for variant, source, occurrences in cases:
            with self.subTest(variant=variant):
                self.write("b.tsx", source)
                report, _ = self.invoke(0)
                self.assertEqual(report["diagnostics"], [])
                self.assertEqual(report["files_scanned"], 2)
                expected = [] if occurrences is None else [("source-duplication", 13, occurrences)]
                self.assertEqual([
                    (item["rule"], item["score"],
                     [(occurrence["path"], span(occurrence)) for occurrence in item["occurrences"]])
                    for item in report["findings"]
                ], expected)

    def test_source_duplication_baseline_is_pair_and_content_specific(self):
        self.duplication("baseline")
        long = "prepare(item); send(item); cleanup(item);"
        shorter = "send(item); cleanup(item);"

        def body(name, statements):
            return f"function {name}() {{ {statements} }}"

        self.write("a.ts", body("first", long))
        self.write("b.ts", body("second", long))
        self.write(BASELINE, '{"scoring_version":1,"entries":[]}')
        self.assertEqual(self.invoke(1)[0]["findings"][0]["status"], "blocking")
        baseline, raw = self.invoke(0, "baseline")
        self.assertEqual(len(baseline["duplication_entries"]), 1)
        entry, = baseline["duplication_entries"]
        self.assertEqual(([item["path"] for item in entry["occurrences"]], entry["score"]),
                         (["a.ts", "b.ts"], 15))
        self.write(BASELINE, raw.decode())

        def statuses(code):
            return [(item["score"], item["status"]) for item in self.invoke(code)[0]["findings"]]

        self.assertEqual(statuses(0), [(15, "baselined")])
        self.write("b.ts", "// unrelated\n\n" + body("second", shorter))
        self.assertEqual(statuses(0), [(10, "baselined")])
        for path, name in (("a.ts", "first"), ("b.ts", "second")):
            self.write(path, body(name, long + " verify(item);"))
        self.assertEqual(statuses(1), [(20, "blocking")])
        for path, name in (("a.ts", "first"), ("b.ts", "second")):
            self.write(path, body(name, long.replace("item", "other")))
        self.assertEqual(statuses(1), [(15, "blocking")])

        self.write("a.ts", body("first", long))
        self.write("b.ts", body("second", long))
        self.write("c.ts", body("third", shorter))
        self.assertEqual(statuses(1), [(15, "baselined"), (10, "blocking"), (10, "blocking")])
        (self.root / "c.ts").unlink()
        self.write("b.ts", body("second", long) + body("second", "unrelated();"))
        self.assertEqual(statuses(1), [(15, "blocking")])
        (self.root / "b.ts").rename(self.root / "moved.ts")
        self.write("moved.ts", body("second", long))
        self.assertEqual(statuses(1), [(15, "blocking")])
        self.write("moved.ts", body("other", long + " different(); " + shorter))
        self.invoke(1)

    def test_source_duplication_consolidates_repeated_runs(self):
        self.duplication(minimum=20)
        self.write("copies.ts", "function both() {" + "ping();" * 2000 + "}")
        report, _ = self.invoke(0)
        self.assertLess(len(report["findings"]), 20)
        largest = max(report["findings"], key=lambda item: item["score"])
        self.assertEqual(largest["score"], 4000)
        self.assertEqual(largest["occurrences"][0]["range"]["end"],
                         largest["occurrences"][1]["range"]["start"])
        self.write("exhausted.ts", "function repeat() {" + "ping();" * 30000 + "}")
        report, _ = self.invoke(2)
        self.assertIn("duplication_limit", [item["kind"] for item in report["diagnostics"]])
        self.assertEqual(self.invoke(2, "baseline")[1], b"")

    def test_source_duplication_configuration_and_failed_parse(self):
        self.config(limits={}, duplication={})
        self.write("a.ts", "function a() {" + "unique();" * 13 + "}")
        self.write("b.tsx", "function b() {" + "unique();" * 13 + "}")
        report, _ = self.invoke(0)
        largest = max(report["findings"], key=lambda item: item["score"])
        self.assertEqual((largest["score"], largest["limit"]), (52, 50))
        self.config(limits={}, duplication={"minimum_tokens": 0})
        self.assertEqual(self.invoke(2, "validate")[0]["diagnostics"][0]["kind"], "invalid_config")
        self.duplication()
        self.write("broken.tsx", "const malformed = <main>")
        self.assertIn("parse_error", [item["kind"] for item in self.invoke(2)[0]["diagnostics"]])

    def test_mixed_fixtures_match_hand_scored_contract(self):
        shutil.copytree(HERE / "fixtures/mixed", self.root, dirs_exist_ok=True)
        report, raw = self.invoke(0)
        self.assertEqual((report["format_version"], report["scoring_version"]), (2, 1))
        self.assertEqual((report["files_scanned"], report["files_skipped"]), (4, 2))
        self.assertEqual(report["diagnostics"], [])
        expected = [
            ("flow.py", "sample", 8, (2, 5), (11, 22)),
            ("flow.py", "nested", 2, (3, 18), (3, 41)),
            ("flow.rs", "sample", 8, (1, 1), (12, 2)),
            ("flow.rs", "nested", 2, (2, 18), (2, 42)),
            ("flow.ts", "sample", 5, (1, 1), (7, 2)),
            ("flow.ts", "nested", 2, (2, 20), (2, 35)),
            ("view.tsx", "View", 3, (1, 21), (3, 2)),
        ]

        self.assertEqual([
            (item["path"], item["name"], item["score"], *span(item))
            for item in report["findings"]
        ], expected)
        for item in report["findings"]:
            self.assertEqual((item["rule"], item["status"], item["limit"]),
                             ("cyclomatic-complexity", "reported", 1))
        self.assertEqual(report["findings"][1]["scope"][-2]["name"], "sample")
        paths = [path.name for path in self.root.iterdir()]
        self.assertEqual(self.invoke(0, "scan", "--", *reversed(paths), "flow.ts")[1], raw)

    def test_python_match_guards_and_nested_comprehensions(self):
        self.config(limits={"python": 1})
        self.write("guards.py", """def collect(groups, allow):
    match groups:
        case [first, *rest] if first and (rest or allow):
            def keep(item):
                return any(part for part in item if part)
            return [[item for item in group if keep(item)] for group in rest if group]
        case _:
            return []
""")
        report, _ = self.invoke(0)
        self.assertEqual(report["diagnostics"], [])
        # collect: 1 base + 2 cases + 1 guard + and + or + 2 comprehension fors + 2 ifs.
        # keep: 1 base + 1 generator for + 1 generator if.
        self.assertEqual([
            (item["rule"], item["path"], tuple(scope["name"] for scope in item["scope"]),
             item["score"], span(item))
            for item in report["findings"]
        ], [
            ("cyclomatic-complexity", "guards.py", ("collect",), 10, ((1, 1), (8, 22))),
            ("cyclomatic-complexity", "guards.py", ("collect", "keep"), 3, ((4, 13), (5, 58))),
        ])

    def test_additional_grammar_constructs(self):
        self.write("choices.rs", """impl Item {
    fn choices(&self, x: Option<bool>) {
        let Some(y) = x else { return; };
        while let Some(a) = x && a { break; }
        for _ in [1] { continue; }
        loop { break; }
        if y || !y {} else if y {}
        ignore!(if a || b {});
    }
}
""")
        self.write("choices.ts", """class Example {
    *choices({a = true} = {}) {
        for (const item of []) {}
        for (;;) { break; }
        while (a) { break; }
        do {} while (a);
        try { a &&= a; a ||= a; a ??= false; }
        catch { return this?.call?.()[0]?.[1]; }
    }
}
""")
        self.write("choices.py", """async def choices(items):
    async for x in items:
        if x:
            continue
        elif not x:
            break
    while items:
        try:
            raise ExceptionGroup("x", [])
        except* ValueError:
            pass
    return (a for a in items for b in items if a or b)
""")
        self.write("unicode.ts", """const é = "if && ??"; consume(() => false || true); // if
""")
        report, _ = self.invoke(0)
        self.assertEqual(
            [(item["name"], item["score"]) for item in report["findings"]],
            [("choices", 10), ("choices", 9), ("choices", 14), ("<anonymous>", 2)],
        )
        anonymous = report["findings"][-1]
        self.assertEqual(anonymous["range"]["start"]["column"],
                         len('const é = "if && ??"; consume('.encode("utf-8")) + 1)

    def test_cognitive_nesting_and_callable_boundaries(self):
        self.config(limits={"typescript": 1, "tsx": 1},
                    cognitive_limits={"typescript": 1, "tsx": 1})
        self.write("scopes.ts", """function nested(a: boolean, b: boolean) {
    if (a) { if (b) work(); }
}
function guards(a: boolean, b: boolean) {
    if (!a) return;
    if (!b) return;
    work();
}
function host(a: boolean) {
    if (a) {
        consume(() => { if (a) { if (a) work(); } });
    }
}\n""")
        self.write("view.tsx", """const view = () => <p>{ok ? (small ? "s" : "m") : "l"}</p>;
""")
        report, _ = self.invoke(0)
        self.assertEqual(report["diagnostics"], [])
        cognitive = [item for item in report["findings"] if item["rule"] == "cognitive-complexity"]
        self.assertEqual([(item["name"], item["score"]) for item in cognitive],
                         [("nested", 3), ("guards", 2), ("<anonymous>", 3), ("view", 3)])
        contributions = cognitive[0]["contributions"]
        self.assertEqual([(p["construct"], p["points"], p["nesting"]) for p in contributions],
                         [("if", 1, 0), ("if", 2, 1)])
        self.assertEqual(contributions[1]["range"]["start"], {"line": 2, "column": 14})
        cyclomatic = [item for item in report["findings"] if item["rule"] == "cyclomatic-complexity"]
        self.assertEqual([(item["name"], item["score"]) for item in cyclomatic[:2]],
                         [("nested", 3), ("guards", 3)])

    def test_cognitive_control_flow_and_logical_runs(self):
        self.config(limits={"typescript": 100}, cognitive_limits={"typescript": 1})
        self.write("flow.ts", """function example(a: any, b: any, c: any, d: any, e: any) {
    if (a && (b && c) || d) {
        outer: for (const item of e) {
            if (item?.x ?? false) break outer;
        }
    } else if (b) {
        while (c) break;
    } else {
        try {
            switch (a) {
                case 0: return b ? 0 : 1;
                case 1: return 2;
                default: return 3;
            }
        } catch {
            if (c) throw c;
        } finally {
            return;
        }
    }
}\n""")
        report, _ = self.invoke(0)
        self.assertEqual(report["diagnostics"], [])
        finding, = report["findings"]
        expected = [
            ("if", 1), ("&&", 1), ("||", 1), ("for", 2), ("if", 3), ("??", 1),
            ("labeled break", 1), ("else if", 1), ("while", 2), ("else", 1),
            ("switch", 2), ("ternary", 3), ("catch", 2), ("if", 3),
        ]
        self.assertEqual([(p["construct"], p["points"]) for p in finding["contributions"]], expected)
        self.assertEqual(finding["score"], 24)

    def test_typescript_optional_hops_and_logical_runs(self):
        self.config(limits={"typescript": 1}, cognitive_limits={"typescript": 1})
        self.write("optional.ts", """function load() {
    return ((source?.client?.load?.() && (ready && cached)) || backup) ?? select(value ?? fallback);
}
""")
        report, _ = self.invoke(0)
        self.assertEqual(report["diagnostics"], [])
        # Cyclomatic: 1 base + 3 optional hops + 2 && + 1 || + 2 ??.
        # Cognitive: one && run across parentheses, one || run, and two ?? runs.
        # The call separates the argument's ?? from the surrounding expression.
        self.assertEqual([
            (item["rule"], item["path"], item["name"], item["score"], span(item))
            for item in report["findings"]
        ], [
            ("cyclomatic-complexity", "optional.ts", "load", 9, ((1, 1), (3, 2))),
            ("cognitive-complexity", "optional.ts", "load", 4, ((1, 1), (3, 2))),
        ])
        self.assertEqual([
            (part["construct"], part["points"], part["nesting"], span(part))
            for part in report["findings"][1]["contributions"]
        ], [
            ("&&", 1, 0, ((2, 39), (2, 41))),
            ("||", 1, 0, ((2, 61), (2, 63))),
            ("??", 1, 0, ((2, 72), (2, 74))),
            ("??", 1, 0, ((2, 88), (2, 90))),
        ])

    def test_both_defaults_are_twelve_and_overrides_are_independent(self):
        branches = "if (x) work();"
        self.write("limits.ts",
                   f"function cyclo_equal() {{ {branches * 11} }}\n"
                   f"function cognitive_equal() {{ {branches * 12} }}\n"
                   f"function both_over() {{ {branches * 13} }}\n")
        self.write(POLICY, '{"mode": "strict"}')
        report, _ = self.invoke(1)
        self.assertEqual([(f["name"], f["rule"], f["score"], f["limit"]) for f in report["findings"]],
                         [("cognitive_equal", "cyclomatic-complexity", 13, 12),
                          ("both_over", "cyclomatic-complexity", 14, 12),
                          ("both_over", "cognitive-complexity", 13, 12)])
        self.config("strict", {"typescript": 100})
        report, _ = self.invoke(1)
        self.assertEqual([(f["rule"], f["limit"]) for f in report["findings"]],
                         [("cognitive-complexity", 12)])
        self.write(POLICY, '{"mode": "strict", "cognitive_limits": {"typescript": 100}}')
        report, _ = self.invoke(1)
        self.assertEqual([(f["rule"], f["limit"]) for f in report["findings"]],
                         [("cyclomatic-complexity", 12)] * 2)
        for invalid in ({"typescript": 0}, {"python": 12}, {"tsx": 12}):
            self.config(limits={"typescript": 12}, cognitive_limits=invalid)
            self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "invalid_config")

    def test_baseline_exceptions_are_per_rule(self):
        self.config("baseline", {"typescript": 1}, cognitive_limits={"typescript": 1})
        self.write("debt.ts", "function debt() { if (a) { if (b) work(); } }")
        baseline, _ = self.invoke(0, "baseline")
        cyclomatic, cognitive = baseline["entries"]
        self.assertEqual([cyclomatic["rule"], cognitive["rule"]],
                         ["cyclomatic-complexity", "cognitive-complexity"])
        self.assertEqual(cyclomatic["identity"], cognitive["identity"])
        for entries, code, expected in (
            ([cyclomatic, cognitive], 0, ["baselined", "baselined"]),
            ([{key: value for key, value in cyclomatic.items() if key != "rule"}],
             1, ["baselined", "blocking"]),
            ([cognitive], 1, ["blocking", "baselined"]),
            ([cyclomatic, cyclomatic, cognitive], 1, ["blocking", "baselined"]),
        ):
            self.write(BASELINE, json.dumps({**baseline, "entries": entries}))
            report, _ = self.invoke(code)
            self.assertEqual([f["status"] for f in report["findings"]], expected)

    def test_strict_threshold_and_selection(self):
        self.write("src/choices.ts", "const choose = (x: boolean) => x ? 1 : 0;")
        self.write("src/also.tsx", "this is invalid TSX")
        self.write("src/skipped/choices.ts", "function {")
        self.write("elsewhere.ts", "function {")
        self.config("strict", {"typescript": 2}, include=["src/**"], exclude=["src/skipped/**"])
        report, _ = self.invoke(0)
        self.assertEqual((report["files_scanned"], report["findings"]), (1, []))
        self.assertEqual(self.invoke(2, "scan", "src/skipped/choices.ts")[0]["diagnostics"][0]["kind"],
                         "empty_scan")
        self.config("strict", {"typescript": 1}, include=["src/**"], exclude=["src/skipped/**"])
        report, _ = self.invoke(1)
        self.assertEqual(report["findings"][0]["status"], "blocking")

    def test_analysis_failures_override_report_and_gate_modes(self):
        for limits in ({}, {"typescript": 0}, {"javascript": 20}):
            self.config(limits=limits)
            self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "invalid_config")
        self.config(include=["../**"])
        self.assertEqual(self.invoke(2, "validate")[0]["diagnostics"][0]["kind"], "invalid_config")
        self.config(include=["["])
        self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "invalid_config")
        self.config()
        self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "empty_scan")
        self.assertEqual(self.invoke(2, "scan", "../escape.ts")[0]["diagnostics"][0]["kind"],
                         "invalid_path")
        self.assertEqual(self.invoke(2, "scan", "absent.ts")[0]["diagnostics"][0]["kind"],
                         "unreadable_source")
        self.write("good.ts", "function f(x: boolean) { return x ? 1 : 0; }")
        self.write("broken.ts", "function f() { return 0;")
        self.write("syntax.py", "def :\n\tclass")
        (self.root / "encoding.ts").write_bytes(b"\xff")
        with (self.root / "large.ts").open("wb") as stream:
            stream.truncate(4 * 1024 * 1024 + 1)
        for mode in ("report-only", "strict"):
            self.config(mode)
            report, _ = self.invoke(2)
            kinds = {(item["path"], item["kind"]) for item in report["diagnostics"]}
            self.assertEqual(kinds, {
                ("broken.ts", "parse_error"), ("syntax.py", "parse_error"),
                ("encoding.ts", "invalid_encoding"), ("large.ts", "source_too_large"),
            })
            self.assertTrue(any("expected" in item["message"] for item in report["diagnostics"]))
            self.assertEqual([item["name"] for item in report["findings"]], ["f"])
        self.invoke(2, "baseline")

    def test_regular_files_and_symlinks(self):
        self.write("real/ready.py", "def ready(): return True\n")
        (self.root / "link").symlink_to(self.root / "real", target_is_directory=True)
        (self.root / "link.py").symlink_to(self.root / "real/ready.py")
        report, _ = self.invoke(0)
        self.assertEqual(report["files_scanned"], 1)
        self.assertEqual(self.invoke(2, "scan", "link/ready.py")[0]["diagnostics"][0]["kind"],
                         "invalid_path")
        if os.geteuid() != 0:
            self.write("private.py", "pass")
            (self.root / "private.py").chmod(0)
            self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "unreadable_source")

    def test_baseline_and_candidate_identity(self):
        original = "def f(x):\n    return 1 if x and x > 2 else 0\n"
        self.write("debt.py", original)
        self.config("baseline", {"python": 1})
        self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "invalid_baseline")

        baseline, _ = self.invoke(0, "baseline")
        self.assertEqual(baseline["entries"], [{
            "rule": "cyclomatic-complexity",
            "identity": {"path": "debt.py", "language": "python",
                         "scope": [{"kind": "function", "name": "f"}]},
            "score": 3,
        }])
        self.write(BASELINE, json.dumps(baseline))
        self.invoke(0, "validate")

        cases = [
            ("\n" + original, 0, [(3, "baselined")]),
            (original.replace(" and x > 2", ""), 0, [(2, "baselined")]),
            (original.replace(" and x > 2", " and x > 2 or x"), 1, [(4, "blocking")]),
            (original + original.replace("def f", "def g"), 1,
             [(3, "baselined"), (3, "blocking")]),
            (original + "def f(x): return x\n", 1, [(3, "blocking")]),
        ]
        for source, code, expected in cases:
            self.write("debt.py", source)
            report, _ = self.invoke(code)
            self.assertEqual([(item["score"], item["status"]) for item in report["findings"]], expected)
        self.assertEqual(self.invoke(0, "baseline")[0]["entries"], [])

        self.write("debt.py", original)
        baseline["entries"] *= 2
        self.write(BASELINE, json.dumps(baseline))
        self.assertEqual(self.invoke(1)[0]["findings"][0]["status"], "blocking")
        baseline["entries"].pop()
        self.write(BASELINE, json.dumps(baseline))
        (self.root / "debt.py").rename(self.root / "moved.py")
        self.assertEqual(self.invoke(1)[0]["findings"][0]["status"], "blocking")
        baseline["scoring_version"] += 1
        self.write(BASELINE, json.dumps(baseline))
        self.assertEqual(self.invoke(2)[0]["diagnostics"][0]["kind"], "invalid_baseline")

    def test_baseline_review_proposes_only_safe_reductions(self):
        source = "def f(x):\n    return 1 if x and x > 2 else 0\n"
        self.write("debt.py", source)
        self.config("baseline", {"python": 1}, exclude=["excluded/**"])
        baseline, _ = self.invoke(0, "baseline")
        baseline["entries"].append({
            "rule": "cyclomatic-complexity",
            "identity": {"path": "excluded/debt.py", "language": "python",
                         "scope": [{"kind": "function", "name": "hidden"}]},
            "score": 8,
        })
        baseline["entries"].append({
            "rule": "cyclomatic-complexity",
            "identity": {"path": "not-selected.ts", "language": "typescript",
                         "scope": [{"kind": "function", "name": "dormant"}]},
            "score": 5,
        })
        self.write(BASELINE, json.dumps(baseline, indent=2) + "\n")
        before = (self.root / BASELINE).read_bytes()

        self.write("debt.py", source.replace(" and x > 2", ""))
        first = subprocess.run([BIN, "baseline-review", "--root", self.root],
                               capture_output=True, check=False)
        second = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                capture_output=True, check=False)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, second.stdout)
        self.assertIn(b"outside policy selection", first.stderr)
        self.assertIn(b"inactive complexity rule", first.stderr)
        operations = json.loads(first.stdout)
        self.assertEqual(operations, [{
            "op": "replace", "path": "/entries/0/score", "value": 2,
        }])
        self.assertEqual((self.root / BASELINE).read_bytes(), before)

        for operation in operations:
            _, collection, index, *field = operation["path"].split("/")
            target = baseline[collection]
            index = int(index)
            if operation["op"] == "remove":
                target.pop(index)
            else:
                target[index][field[0]] = operation["value"]
        self.write(BASELINE, json.dumps(baseline, indent=2) + "\n")
        noop = subprocess.run([BIN, "baseline-review", "--root", self.root],
                              capture_output=True, check=False)
        self.assertEqual((noop.returncode, noop.stdout), (0, b"[]\n"))
        self.assertIn(b"preserved unchanged or worsened callable", noop.stderr)

        self.write("debt.py", "def f(x): return x\n")
        removal = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                 capture_output=True, check=False)
        self.assertEqual(removal.returncode, 0, removal.stderr)
        self.assertEqual(json.loads(removal.stdout), [{"op": "remove", "path": "/entries/0"}])

        self.write("broken.py", "def broken(:\n")
        incomplete = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                    capture_output=True, check=False)
        self.assertEqual(incomplete.returncode, 2)
        self.assertEqual(incomplete.stdout, b"")

    def test_baseline_review_preserves_entries_under_excluded_directories(self):
        options = {"limits": {"python": 1}, "duplication": {"minimum_tokens": 10}}
        self.config("baseline", **options)
        self.write("src/current.ts", "function current() { prepare(item); send(item); }")
        self.write("vendor/nested/copy.ts", "function copy() { prepare(item); send(item); }")
        self.write("vendor/nested/debt.py", "def f(x): return 1 if x else 0\n")
        baseline, raw = self.invoke(0, "baseline")
        self.assertEqual(len(baseline["entries"]), 1)
        self.assertEqual(len(baseline["duplication_entries"]), 1)
        self.write(BASELINE, raw.decode())

        self.config("baseline", exclude=["vendor"], **options)
        self.assertEqual(self.invoke(0)[0]["files_scanned"], 1)
        self.assertEqual(self.invoke(0, "baseline-review")[0], [])

    def test_baseline_review_preserves_non_enforcing_rule_entries(self):
        options = {"limits": {"python": 1}, "duplication": {"minimum_tokens": 10}}
        self.config("baseline", **options)
        self.write("debt.py", "def f(x): return 1 if x else 0\n")
        self.write("a.ts", "function first() { prepare(item); send(item); }")
        self.write("b.ts", "function second() { prepare(item); send(item); }")
        baseline, raw = self.invoke(0, "baseline")
        self.assertEqual(len(baseline["entries"]), 1)
        self.assertEqual(len(baseline["duplication_entries"]), 1)
        self.write(BASELINE, raw.decode())

        self.write("debt.py", "def f(x): return x\n")
        self.write("b.ts", "function second() { return 0; }")
        for complexity, duplication in [("pass", "warn"), ("warn", "pass")]:
            with self.subTest(complexity=complexity, duplication=duplication):
                self.config("baseline", severities={
                    "cyclomatic-complexity": complexity,
                    "source-duplication": duplication,
                }, **options)
                self.assertEqual(self.invoke(0, "baseline-review")[0], [])

    def test_baseline_review_narrows_shorter_duplication(self):
        self.duplication("baseline", minimum=10)
        long = "prepare(item); send(item); cleanup(item);"
        short = "send(item); cleanup(item);"
        self.write("a.ts", f"function first() {{ {long} }}")
        self.write("b.ts", f"function second() {{ {long} }}")
        baseline, _ = self.invoke(0, "baseline")
        self.write(BASELINE, json.dumps(baseline, indent=2) + "\n")

        self.write("b.ts", f"function second() {{ {short} }}")
        result = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        operations = json.loads(result.stdout)
        self.assertEqual([item["path"].rsplit("/", 1)[-1] for item in operations],
                         ["score", "tokens"])
        self.assertEqual(operations[0]["value"], len(operations[1]["value"]))
        self.assertLess(operations[0]["value"], baseline["duplication_entries"][0]["score"])

    def test_baseline_review_preserves_new_rewritten_worsened_and_ambiguous_duplicates(self):
        self.duplication("baseline", minimum=10)
        original = "prepare(item); send(item); cleanup(item);"
        rewritten = "prepare(other); send(other); cleanup(other);"

        def body(name, statements):
            return f"function {name}() {{ {statements} }}"

        self.write("a.ts", body("first", original))
        self.write("b.ts", body("second", original))
        baseline, _ = self.invoke(0, "baseline")
        self.write(BASELINE, json.dumps(baseline))

        # A third copy is a new pair and must not change the accepted pair.
        self.write("c.ts", body("third", original))
        added_copy = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                    capture_output=True, check=False)
        self.assertEqual((added_copy.returncode, added_copy.stdout), (0, b"[]\n"))
        (self.root / "c.ts").unlink()

        # Replacing tokens in both occurrences keeps the allowance outside the patch.
        self.write("a.ts", body("first", rewritten))
        self.write("b.ts", body("second", rewritten))
        replaced = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                  capture_output=True, check=False)
        self.assertEqual((replaced.returncode, replaced.stdout), (0, b"[]\n"))
        self.assertIn(b"rewritten duplication match", replaced.stderr)

        # Enlarging the match cannot grant or narrow an allowance.
        worsened = original + " finish(item);"
        self.write("a.ts", body("first", worsened))
        self.write("b.ts", body("second", worsened))
        enlarged = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                  capture_output=True, check=False)
        self.assertEqual((enlarged.returncode, enlarged.stdout), (0, b"[]\n"))
        self.assertIn(b"preserved unchanged, worsened, or rewritten duplication match", enlarged.stderr)
        report, _ = self.invoke(1)
        self.assertGreater(report["findings"][0]["score"], baseline["duplication_entries"][0]["score"])

        # Repeated overlapping runs make the callable anchors ambiguous.
        repeated = original + " " + original
        self.write("a.ts", body("first", repeated))
        self.write("b.ts", body("second", repeated))
        ambiguous = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                   capture_output=True, check=False)
        self.assertEqual((ambiguous.returncode, ambiguous.stdout), (0, b"[]\n"))
        self.assertIn(b"deferred", ambiguous.stderr)

    def test_baseline_review_emits_no_patch_on_duplication_resource_error(self):
        self.duplication("baseline", minimum=20)
        self.write("a.ts", "function first() { uniqueA(); }")
        self.write("b.ts", "function second() { uniqueB(); }")
        baseline, _ = self.invoke(0, "baseline")
        self.write(BASELINE, json.dumps(baseline))
        self.write("exhausted.ts", "function repeat() {" + "ping();" * 30000 + "}")
        result = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"duplication_limit", result.stderr)

    def test_baseline_review_explains_inactive_duplication_entries(self):
        self.duplication("baseline", minimum=10)
        self.write("a.ts", "function first() { prepare(item); send(item); }")
        self.write("b.ts", "function second() { prepare(item); send(item); }")
        baseline, _ = self.invoke(0, "baseline")
        self.write(BASELINE, json.dumps(baseline))
        self.config("baseline", {"python": 1})
        self.write("source.py", "def ready(): return True\n")
        result = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                capture_output=True, check=False)
        self.assertEqual((result.returncode, result.stdout), (0, b"[]\n"))
        self.assertIn(b"inactive duplication rule", result.stderr)

    def test_baseline_review_requires_existing_baseline_and_full_scan(self):
        self.write("source.py", "def f(): return 1\n")
        missing = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                 capture_output=True, check=False)
        self.assertEqual(missing.returncode, 2)
        self.assertEqual(missing.stdout, b"")
        missing_policy = subprocess.run(
            [BIN, "baseline-review", "--root", self.root, "--config", self.root / "missing.json"],
            capture_output=True, check=False,
        )
        self.assertEqual(missing_policy.returncode, 2)
        self.assertEqual(missing_policy.stdout, b"")
        baseline, _ = self.invoke(0, "baseline")
        self.write(BASELINE, json.dumps(baseline))
        baseline["scoring_version"] += 1
        self.write(BASELINE, json.dumps(baseline))
        incompatible = subprocess.run([BIN, "baseline-review", "--root", self.root],
                                     capture_output=True, check=False)
        self.assertEqual(incompatible.returncode, 2)
        self.assertEqual(incompatible.stdout, b"")
        baseline["scoring_version"] -= 1
        self.write(BASELINE, json.dumps(baseline))
        partial = subprocess.run([BIN, "baseline-review", "--root", self.root, "--", "source.py"],
                                 capture_output=True, check=False)
        self.assertNotEqual(partial.returncode, 0)
        self.assertEqual(partial.stdout, b"")

    def test_unique_returned_callable_can_be_baselined(self):
        self.config("baseline", {"typescript": 1})
        self.write("factory.ts", "function build() { return () => a || b; }")
        baseline, _ = self.invoke(0, "baseline")
        self.assertEqual(baseline["entries"][0]["identity"]["scope"], [
            {"kind": "function", "name": "build"}, {"kind": "arrow", "name": "<return>"},
        ])
        self.write(BASELINE, json.dumps(baseline))
        self.assertEqual(self.invoke(0)[0]["findings"][0]["status"], "baselined")
        self.write("factory.ts", "function build() { if (c) return () => a || b; return () => c; }")
        findings = self.invoke(1)[0]["findings"]
        self.assertEqual([(item["name"], item["status"]) for item in findings],
                         [("build", "blocking"), ("<return>", "blocking")])

    def test_anonymous_callables_have_no_unsafe_waiver(self):
        self.config("baseline", {"typescript": 1})
        self.write("callback.ts", "consume(() => a || b);")
        baseline, _ = self.invoke(0, "baseline")
        self.assertEqual(baseline["entries"], [])
        self.write(BASELINE, json.dumps(baseline))
        self.assertEqual(self.invoke(1)[0]["findings"][0]["status"], "blocking")

    def test_rule_severity_matrix_and_pass_skips_analysis(self):
        self.write("flow.ts", "function complex() { if (a) return b; else return c; }")
        self.write(BASELINE, json.dumps({"scoring_version": 1, "entries": []}))
        for mode, severity, code, status in (
            ("report-only", "error", 0, "reported"),
            ("strict", "error", 1, "blocking"),
            ("baseline", "error", 1, "blocking"),
            ("strict", "warn", 0, "reported"),
            ("baseline", "warn", 0, "reported"),
            ("report-only", "warn", 0, "reported"),
        ):
            self.config(mode, {"typescript": 1}, severities={"cyclomatic-complexity": severity})
            report, _ = self.invoke(code)
            finding, = report["findings"]
            self.assertEqual((finding["severity"], finding["status"]), (severity, status))

        self.config("strict", {"typescript": 1}, severities={
            "cyclomatic-complexity": "pass", "cognitive-complexity": "pass",
        })
        report, _ = self.invoke(0)
        self.assertEqual((report["findings"], report["diagnostics"]), ([], []))
        self.assertEqual(report["files_scanned"], 0)

        self.config("strict", {"typescript": 1}, severities={
            "cyclomatic-complexity": "pass", "cognitive-complexity": "pass",
            "source-duplication": "warn",
        })
        self.assertEqual(self.invoke(0)[0]["findings"], [])

    def test_pass_skips_duplication_budget_and_validation_rejects_bad_severity(self):
        self.write("copies.ts", "function repeat() {" + "ping();" * 30000 + "}")
        self.write("complex.ts", "function choose() { return b ? a : c; }")
        self.config("strict", limits={"typescript": 1}, duplication={}, severities={
            "cyclomatic-complexity": "warn", "source-duplication": "pass",
        })
        report, _ = self.invoke(0)
        self.assertEqual(report["diagnostics"], [])
        self.assertEqual(report["files_scanned"], 2)
        self.assertEqual([item["path"] for item in report["findings"]], ["complex.ts"])
        self.config("strict", {"typescript": 1}, severities={"cyclomatic-complexity": "invalid"})
        self.assertEqual(self.invoke(2, "validate")[0]["diagnostics"][0]["kind"], "invalid_config")
        self.config("strict", {"typescript": 1}, severities={"unknown-rule": "warn"})
        self.assertEqual(self.invoke(2, "validate")[0]["diagnostics"][0]["kind"], "invalid_config")

    def test_all_pass_validates_explicit_paths_without_walking_directories(self):
        self.config("strict", limits={"python": 1}, severities={"cyclomatic-complexity": "pass"})
        inaccessible = self.root / "hidden"
        inaccessible.mkdir()
        inaccessible.chmod(0)
        try:
            for paths in ((), ("--", "hidden")):
                report, _ = self.invoke(0, "scan", *paths)
                self.assertEqual((report["files_scanned"], report["diagnostics"]), (0, []))
            report, _ = self.invoke(2, "scan", "--", "missing")
            self.assertEqual(report["diagnostics"][0]["kind"], "unreadable_source")
        finally:
            inaccessible.chmod(0o700)

    def test_duplication_token_exhaustion_still_reports_later_parse_errors(self):
        self.duplication("strict", severities={"source-duplication": "warn"})
        self.write("a.ts", "function repeat() {" + "ping();" * 210000 + "}")
        self.write("b.ts", "function (")
        report, _ = self.invoke(2)
        self.assertEqual(report["files_scanned"], 2)
        self.assertEqual([item["kind"] for item in report["diagnostics"]],
                         ["duplication_limit", "parse_error"])

    def test_warning_preview_and_error_promotion_baseline_generation(self):
        self.write("flow.ts", "function complex() { if (a) return b; else return c; }")
        self.config("baseline", {"typescript": 1}, severities={"cyclomatic-complexity": "warn"})
        empty, raw = self.invoke(0, "baseline")
        self.assertEqual(empty["entries"], [])
        self.write(BASELINE, raw.decode())
        warning = self.invoke(0)[0]["findings"][0]
        self.assertEqual((warning["severity"], warning["status"]), ("warn", "reported"))

        self.config("baseline", {"typescript": 1}, severities={"cyclomatic-complexity": "error"})
        self.assertEqual(self.invoke(1)[0]["findings"][0]["status"], "blocking")
        promoted, raw = self.invoke(0, "baseline")
        self.assertEqual(len(promoted["entries"]), 1)
        self.write(BASELINE, raw.decode())
        finding = self.invoke(0)[0]["findings"][0]
        self.assertEqual((finding["severity"], finding["status"]), ("error", "baselined"))

    def test_baseline_regeneration_keeps_exceptions_for_pass_and_warn_rules(self):
        self.write("a.ts", "function a() { if (a) { if (b) run(); } prepare(item); send(item); }")
        self.write("b.ts", "function b() { prepare(item); send(item); }")
        limits = {"typescript": 1}
        cognitive = {"typescript": 1}
        duplication = {"minimum_tokens": 10}
        self.config("report-only", limits, cognitive_limits=cognitive, duplication=duplication,
                    severities={"cognitive-complexity": "warn"})
        original, raw = self.invoke(0, "baseline")
        self.assertEqual([item["rule"] for item in original["entries"]], ["cyclomatic-complexity"])
        self.assertEqual(len(original["duplication_entries"]), 1)
        self.write(BASELINE, raw.decode())

        self.config("report-only", limits, cognitive_limits=cognitive, duplication=duplication,
                    severities={"cyclomatic-complexity": "warn", "source-duplication": "pass"})
        replacement, raw = self.invoke(0, "baseline")
        self.assertEqual([item["rule"] for item in replacement["entries"]],
                         ["cyclomatic-complexity", "cognitive-complexity"])
        self.assertEqual(replacement["entries"][0], original["entries"][0])
        self.assertEqual(replacement["duplication_entries"], original["duplication_entries"])
        self.write(BASELINE, raw.decode())
        self.config("baseline", limits, cognitive_limits=cognitive, duplication=duplication)
        self.assertEqual([item["status"] for item in self.invoke(0)[0]["findings"]],
                         ["baselined"] * 3)


if __name__ == "__main__":
    unittest.main()
