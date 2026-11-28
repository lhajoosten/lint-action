#[allow(dead_code)]
mod common;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(common::binary())
        .args(args)
        .arg("--root")
        .arg(root)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn physical_body_lines_across_languages_and_thresholds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = |limit| {
        json!({"mode":"strict", "limits":{}, "callable_line_limits":{"rust":limit,"typescript":limit,"tsx":limit,"python":limit}}).to_string()
    };
    fs::write(root.join(".lint-action.json"), config(3)).unwrap();
    for (file, text) in [
        (
            "one.rs",
            "fn long() {\n let a = 1; let b = 2;\n let c =\n  a + b;\n c\n}\nfn short(x: bool) { if x { return; } }\n",
        ),
        (
            "one.ts",
            "function long() {\n const a = 1; const b = 2;\n const c =\n  a + b;\n return c;\n}\nfunction short(x: boolean) { if (x) { return; } }\n",
        ),
        (
            "one.tsx",
            "function long() {\n const a = 1; const b = 2;\n const c =\n  a + b;\n return <div>{c}</div>;\n}\nfunction short(x: boolean) { if (x) { return; } }\n",
        ),
        (
            "one.py",
            "def long():\n    a = 1; b = 2\n    c = (\n        a + b)\n    return c\ndef short(x):\n    if x: return 1\n",
        ),
    ] {
        fs::write(root.join(file), text).unwrap();
    }
    let (status, report) = run(root, &["scan"]);
    assert_eq!(status, 1, "{report}");
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 4, "{report}");
    for item in findings {
        assert_eq!(
            (
                item["rule"].as_str(),
                item["score"].as_u64(),
                item["limit"].as_u64()
            ),
            (Some("callable-lines"), Some(4), Some(3))
        );
    }
    fs::write(root.join(".lint-action.json"), config(4)).unwrap();
    assert_eq!(run(root, &["scan"]).0, 0);
    let path = root.join("one.ts");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace(" const a", " // comment\n\n const a");
    fs::write(path, source).unwrap();
    fs::write(root.join(".lint-action.json"), config(3)).unwrap();
    let report = run(root, &["scan"]).1;
    let item = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["path"] == "one.ts")
        .unwrap();
    assert_eq!(item["score"], 4);
}

#[test]
fn baseline_worsening_and_comparison() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("base");
    let candidate = temp.path().join("candidate");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    let policy =
        json!({"mode":"baseline", "limits":{}, "callable_line_limits":{"python":1}}).to_string();
    fs::write(base.join(".lint-action.json"), &policy).unwrap();
    fs::write(base.join("f.py"), "def f():\n    a = 1\n    return a\n").unwrap();
    let (status, baseline) = run(&base, &["baseline"]);
    assert_eq!(status, 0);
    fs::write(
        base.join(".lint-action-baseline.json"),
        baseline.to_string(),
    )
    .unwrap();
    fs::write(
        candidate.join("f.py"),
        "def f():\n    a = 1\n    a += 1\n    return a\n",
    )
    .unwrap();
    let output = Command::new(common::binary())
        .args(["compare", "--root"])
        .arg(&candidate)
        .arg("--base-root")
        .arg(&base)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["findings"][0]["score"], 3);
    assert_eq!(report["findings"][0]["status"], "blocking");
    assert!(
        report["comparison"].to_string().contains("worsened"),
        "{report}"
    );
}

#[test]
fn literals_nested_bodies_and_expression_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join(".lint-action.json"),
        json!({"mode":"strict", "limits":{}, "callable_line_limits":{"python":1,"typescript":1}})
            .to_string(),
    )
    .unwrap();
    fs::write(root.join("nested.py"), "def outer():\n    \"\"\"first\n    second\n    \"\"\"\n    def inner():\n        return 1\n    return 2\n").unwrap();
    fs::write(root.join("nested.ts"), "function outer() {\n const message = `first\n second`;\n function inner() {\n  return 1;\n }\n return message;\n}\nconst expr = () => one +\n two;\n").unwrap();
    let (_, report) = run(root, &["scan"]);
    let items = report["findings"].as_array().unwrap();
    for (path, name, score) in [
        ("nested.py", "outer", 4),
        ("nested.ts", "outer", 3),
        ("nested.ts", "expr", 2),
    ] {
        let item = items
            .iter()
            .find(|item| item["path"] == path && item["name"] == name)
            .unwrap_or_else(|| panic!("{report}"));
        assert_eq!(item["score"], score, "{report}");
    }
}

#[test]
fn mixed_language_action_report_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join(".lint-action.json"), json!({"mode":"report-only", "limits":{}, "callable_line_limits":{"rust":2,"typescript":2,"tsx":2,"python":2}}).to_string()).unwrap();
    for (path, source) in [
        (
            "logic.rs",
            "fn work() {\n first();\n second();\n third();\n}\n",
        ),
        (
            "logic.ts",
            "function work() {\n first();\n second();\n third();\n}\nfunction compact() { if (a && b) { first(); } }\n",
        ),
        (
            "view.tsx",
            "function View() {\n return <div>\n <span>A</span>\n <span>B</span>\n </div>;\n}\n",
        ),
        (
            "logic.py",
            "def work():\n    first()\n    second()\n    third()\n",
        ),
    ] {
        fs::write(root.join(path), source).unwrap();
    }
    let out = root.join("output");
    let summary = root.join("summary");
    let output = Command::new(common::binary())
        .arg("github-scan")
        .env("GITHUB_EVENT_NAME", "push")
        .env("GITHUB_WORKSPACE", root)
        .env("RUNNER_TEMP", root)
        .env("GITHUB_OUTPUT", &out)
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outputs = fs::read_to_string(out).unwrap();
    let report_path = outputs
        .lines()
        .find_map(|line| line.strip_prefix("report="))
        .unwrap();
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 4, "{report}");
    for (path, score) in [
        ("logic.rs", 3),
        ("logic.ts", 3),
        ("logic.py", 3),
        ("view.tsx", 4),
    ] {
        let finding = findings.iter().find(|v| v["path"] == path).unwrap();
        assert_eq!(finding["score"], score);
        assert_eq!(finding["status"], "reported");
    }
    assert!(
        fs::read_to_string(summary)
            .unwrap()
            .contains("callable lines")
    );
}
