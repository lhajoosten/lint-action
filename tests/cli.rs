//! Replay the packaged CLI's contracts against named fixtures.
mod common;

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Step {
    files: BTreeMap<String, Option<Entry>>,
    args: Vec<String>,
    status: i32,
    stdout: Value,
    stderr: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Text {
        text: String,
    },
    Repeated {
        prefix: String,
        repeat: String,
        count: usize,
        suffix: String,
    },
    Bytes {
        bytes: Vec<u8>,
    },
    Zeroes {
        zero_bytes: u64,
    },
    Symlink {
        symlink: String,
    },
    Directory {
        directory: bool,
        mode: Option<u32>,
    },
    Unreadable {
        unreadable: bool,
        mode: u32,
    },
}

fn substitute(text: &str, trees: &[PathBuf; 2]) -> String {
    text.replace("$TREE0", &trees[0].to_string_lossy())
        .replace("$TREE1", &trees[1].to_string_lossy())
}

fn normalized(bytes: &[u8], trees: &[PathBuf; 2]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    let mut text = String::from_utf8(bytes.to_vec()).unwrap();
    for (index, path) in trees.iter().enumerate() {
        text = text.replace(&*path.to_string_lossy(), &format!("$TREE{index}"));
    }
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

#[cfg(unix)]
fn permissions(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn permissions(_: &Path, _: u32) {}

fn prepare(trees: &[PathBuf; 2], entries: &BTreeMap<String, Option<Entry>>) {
    for (name, value) in entries.iter().rev() {
        if value.is_none() {
            let path = PathBuf::from(substitute(name, trees));
            if path.symlink_metadata().unwrap().is_dir() {
                fs::remove_dir(path).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
    }
    for (name, value) in entries {
        let Some(value) = value else { continue };
        let path = PathBuf::from(substitute(name, trees));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        match value {
            Entry::Text { text } => fs::write(path, text).unwrap(),
            Entry::Repeated {
                prefix,
                repeat,
                count,
                suffix,
            } => {
                fs::write(path, format!("{prefix}{}{suffix}", repeat.repeat(*count))).unwrap();
            }
            Entry::Bytes { bytes } => fs::write(path, bytes).unwrap(),
            Entry::Zeroes { zero_bytes } => {
                File::create(path).unwrap().set_len(*zero_bytes).unwrap()
            }
            Entry::Directory { directory, mode } => {
                assert!(*directory);
                fs::create_dir_all(&path).unwrap();
                if let Some(mode) = mode {
                    permissions(&path, *mode);
                }
            }
            Entry::Unreadable { unreadable, mode } => {
                assert!(*unreadable);
                fs::write(&path, "").unwrap();
                permissions(&path, *mode);
            }
            Entry::Symlink { symlink } => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(substitute(symlink, trees), path).unwrap();
                #[cfg(not(unix))]
                let _ = symlink;
            }
        }
    }
}

fn verify_case(name: &str, fixture: &str, tools: &tempfile::TempDir) {
    #[cfg(not(unix))]
    if matches!(
        name,
        "regular_files_and_symlinks"
            | "all_pass_validates_explicit_paths_without_walking_directories"
    ) {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let trees = [
        directory.path().join("candidate"),
        directory.path().join("base"),
    ];
    for path in &trees {
        fs::create_dir(path).unwrap();
    }
    let steps: Vec<Step> = serde_json::from_str(fixture).unwrap();
    for (index, step) in steps.iter().enumerate() {
        prepare(&trees, &step.files);
        // Root can read chmod 000 files, even on a correctly functioning CLI.
        if step.files.iter().any(|(name, entry)| {
            matches!(entry, Some(Entry::Unreadable { .. }))
                && File::open(substitute(name, &trees)).is_ok()
        }) {
            continue;
        }
        let output = common::command(tools)
            .args(step.args.iter().map(|arg| substitute(arg, &trees)))
            .output()
            .unwrap();
        let context = format!("{name}, step {} ({:?})", index + 1, step.args);
        assert_eq!(
            output.status.code(),
            Some(step.status),
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            normalized(&output.stdout, &trees),
            step.stdout,
            "{context}: stdout"
        );
        assert_eq!(
            normalized(&output.stderr, &trees),
            step.stderr,
            "{context}: stderr"
        );
    }
    // A non-root test user must be able to traverse this directory during cleanup.
    let hidden = trees[0].join("hidden");
    if hidden.exists() {
        permissions(&hidden, 0o700);
    }
}

#[test]
fn packaged_cli_contracts() {
    let tools = common::without_python();
    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli");
    let mut cases: Vec<_> = fs::read_dir(cases)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    cases.sort();
    for path in cases {
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
            continue;
        }
        verify_case(
            path.file_stem().unwrap().to_str().unwrap(),
            &fs::read_to_string(&path).unwrap(),
            &tools,
        );
    }
}

#[test]
fn python_stubs_and_deterministic_output() {
    let tools = common::without_python();
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join(".lint-action.json"),
        json!({"mode": "report-only", "limits": {"python": 1}}).to_string(),
    )
    .unwrap();
    fs::write(
        source.path().join("z.pyi"),
        "def choose(x): return 1 if x else 2\n",
    )
    .unwrap();
    fs::write(
        source.path().join("a.py"),
        "def ready(x): return x or False\n",
    )
    .unwrap();
    let scan = |selection: &[&str]| {
        let result = common::command(&tools)
            .args(["scan", "--root"])
            .arg(source.path())
            .args(selection)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        result.stdout
    };
    let output = scan(&[]);
    assert_eq!(
        output,
        scan(&["--", "z.pyi", "a.py", "z.pyi", ".lint-action.json"])
    );
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["findings"][1]["path"], "z.pyi");
    assert_eq!(report["findings"][1]["language"], "python");
    assert_eq!(report["findings"][1]["score"], 2);
}

#[test]
fn finally_exit_contract() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let tools = common::without_python();
    let run = |args: &[&str], expected| {
        let result = common::command(&tools)
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };
    let config = dir.join(".lint-action.json");
    let policy = json!({"mode":"report-only", "limits":{}, "finally_exit":true});
    fs::write(&config, policy.to_string()).unwrap();
    for ext in ["ts", "tsx"] {
        fs::write(
            dir.join(format!("file.{ext}")),
            "function f() { try { run(); } finally { return 42; } }\n",
        )
        .unwrap();
    }
    fs::write(
        dir.join("file.py"),
        "def f():\n    try: run()\n    finally: return 42\n",
    )
    .unwrap();
    let report = run(&["scan"], 0);
    assert_eq!(report["format_version"], 3);
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3);
    for (item, path, line, start, end) in [
        (&findings[0], "file.py", 3, 14, 23),
        (&findings[1], "file.ts", 1, 41, 51),
        (&findings[2], "file.tsx", 1, 41, 51),
    ] {
        assert_eq!(item["path"], path);
        assert_eq!(item["rule"], "finally-exit");
        assert_eq!(item["status"], "reported");
        assert_eq!(item["transfer"], "return");
        let message = item["message"].as_str().unwrap();
        assert!(message.contains("exception") && message.contains("return"));
        assert_eq!(
            item["range"],
            json!({"start":{"line":line,"column":start},"end":{"line":line,"column":end}})
        );
        assert!(item.get("score").is_none() && item.get("limit").is_none());
    }
    let baseline = run(&["baseline"], 0);
    assert_eq!(baseline["entries"], json!([]));
    fs::write(dir.join(".lint-action-baseline.json"), baseline.to_string()).unwrap();
    let mut policy = policy;
    policy["mode"] = json!("baseline");
    fs::write(config, policy.to_string()).unwrap();
    let enforced = run(&["scan"], 1);
    assert!(
        enforced["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "blocking")
    );
}

#[test]
fn compare_finally_exits_under_base_policy() {
    let base = tempfile::tempdir().unwrap();
    let candidate = tempfile::tempdir().unwrap();
    let tools = common::without_python();
    fs::write(
        base.path().join(".lint-action.json"),
        r#"{"mode":"strict","finally_exit":true,"limits":{}}"#,
    )
    .unwrap();
    fs::write(
        candidate.path().join(".lint-action.json"),
        r#"{"mode":"report-only","finally_exit":false}"#,
    )
    .unwrap();
    let unsafe_source = "def f():\n    try: run()\n    finally: return 0\n";
    let safe_source = "def f():\n    try: run()\n    finally: close()\n";
    for (file, before, after) in [
        ("new.py", safe_source, unsafe_source),
        ("removed.py", unsafe_source, safe_source),
        ("unchanged.py", unsafe_source, unsafe_source),
    ] {
        fs::write(base.path().join(file), before).unwrap();
        fs::write(candidate.path().join(file), after).unwrap();
    }
    let output = common::command(&tools)
        .arg("compare")
        .arg("--root")
        .arg(candidate.path())
        .arg("--base-root")
        .arg(base.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let comparison = &report["comparison"];
    assert_eq!(comparison["format_version"], 2);
    assert_eq!(comparison["unchanged"], 1);
    let changes = comparison["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    for (change, category, side, path) in [
        (&changes[0], "new", "current", "new.py"),
        (&changes[1], "removed", "previous", "removed.py"),
    ] {
        assert_eq!(change["category"], category);
        assert_eq!(change[side]["path"], path);
        assert_eq!(change[side]["transfer"], "return");
        assert!(change[side].get("score").is_none() && change.get("limit").is_none());
    }
}
