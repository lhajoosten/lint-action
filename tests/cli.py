"""Black-box checks for the packaged lint-action executable."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
BIN = Path(os.environ["LINT_ACTION_BIN"]).resolve()
POLICY = ".lint-action.json"
BASELINE = ".lint-action-baseline.json"


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
        self.assertEqual(report["files_scanned"], 4)
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

    def test_source_duplication_groups_by_language_and_baselines_all_families(self):
        self.duplication("baseline")
        samples = {
            "first.rs": "fn first() { prepare(item); send(item); }",
            "second.rs": "fn second() { prepare(item); /* harmless */ send(item); }",
            "first.py": "def first():\n    prepare(item)\n    send(item)\n",
            "second.pyi": "def second():\n  prepare( item ) # harmless\n  send(item)\n",
            "only.ts": "function only() { prepare(item); send(item); }",
        }
        for path, source in samples.items():
            self.write(path, source)
        self.write(BASELINE, '{"scoring_version":1,"entries":[]}')
        expected = [(["first.py", "second.pyi"], 10), (["first.rs", "second.rs"], 10)]
        report, _ = self.invoke(1)
        self.assertEqual([
            ([part["path"] for part in item["occurrences"]], item["score"])
            for item in report["findings"]
        ], expected)
        self.assertEqual(report["files_scanned"], len(samples))
        self.assertTrue(all(item["status"] == "blocking" for item in report["findings"]))
        baseline, raw = self.invoke(0, "baseline")
        self.assertEqual(len(baseline["duplication_entries"]), 2)
        self.write(BASELINE, raw.decode())
        self.write("second.rs", "// unrelated lines\n" + samples["second.rs"])
        self.write("second.pyi", "\n# unrelated lines\n" + samples["second.pyi"])
        self.assertEqual([item["status"] for item in self.invoke(0)[0]["findings"]],
                         ["baselined", "baselined"])

    def test_python_duplication_preserves_suite_boundaries(self):
        self.duplication(minimum=15)
        source = "def first():\n    if ready:\n        prepare(item)\n        send(item)\n"
        self.write("first.py", source)
        self.write("second.py", source.replace("first", "second"))
        report, _ = self.invoke(0)
        self.assertEqual(report["findings"][0]["score"], 16)
        self.write("second.py", source.replace("first", "second").replace("        send", "    send"))
        self.assertEqual(self.invoke(0)[0]["findings"], [])

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
        self.assertEqual((report["format_version"], report["scoring_version"]), (1, 1))
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

        def point(item, edge):
            location = item["range"][edge]
            return location["line"], location["column"]

        self.assertEqual([
            (item["path"], item["name"], item["score"], point(item, "start"), point(item, "end"))
            for item in report["findings"]
        ], expected)
        for item in report["findings"]:
            self.assertEqual((item["rule"], item["status"], item["limit"]),
                             ("cyclomatic-complexity", "reported", 1))
        self.assertEqual(report["findings"][1]["scope"][-2]["name"], "sample")
        paths = [path.name for path in self.root.iterdir()]
        self.assertEqual(self.invoke(0, "scan", "--", *reversed(paths), "flow.ts")[1], raw)

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

    def test_rust_and_python_cognitive_complexity_and_default_limits(self):
        self.write("sample.rs", """fn outer() {
    if ready() {
        for value in values() {
            if valid(value) && clean(value) && visible(value) || forced() {
                use_it(value);
            }
        }
    } else if waiting() {
        while pending() { work(); }
    } else {
        match state() {
            Some(value) if ready() => work(),
            _ => {}
        }
    }
    let Some(value) = input() else { return; };
    let nested = || { if ready() { while waiting() { work(); } } };
    work()?.finish();
}
""")
        self.write("sample.py", """def outer():
    if ready():
        for item in source():
            if (allowed(item) and clean(item)) and valid(item) or force():
                work(item)
    elif waiting():
        while pending():
            work()
    else:
        match state():
            case item if allowed(item):
                work(item)
    try:
        work()
    except ValueError:
        if ready():
            work()
    def nested():
        if ready():
            while waiting():
                work()
    return [choose(x) if allowed(x) else other(x) for x in source() if valid(x)]
""")
        self.write(POLICY, '{"mode":"report-only"}')
        report, _ = self.invoke(0)
        self.assertEqual([(item["language"], item["score"], item["limit"])
                          for item in report["findings"] if item["rule"] == "cognitive-complexity"],
                         [("python", 24, 12), ("rust", 19, 12)])
        self.config("baseline", limits={"rust": 100, "python": 100},
                    cognitive_limits={"rust": 1, "python": 1})
        baseline, raw = self.invoke(0, "baseline")
        self.assertEqual([item["rule"] for item in baseline["entries"]], ["cognitive-complexity"] * 4)
        self.write(BASELINE, raw.decode())
        report, _ = self.invoke(0)
        self.assertEqual([(item["name"], item["score"], item["status"]) for item in report["findings"]],
                         [("outer", 24, "baselined"), ("nested", 3, "baselined"),
                          ("outer", 19, "baselined"), ("nested", 3, "baselined")])
        rust = report["findings"][2]
        self.assertEqual([(item["construct"], item["points"], item["range"]["start"]["line"])
                          for item in rust["contributions"] if item["construct"] in {"guard", "error propagation"}],
                         [("guard", 3, 12), ("error propagation", 1, 18)])

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


if __name__ == "__main__":
    unittest.main()
