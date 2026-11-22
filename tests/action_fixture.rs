#[cfg(unix)]
mod common;

#[cfg(unix)]
mod unix {
    use super::common;
    use serde_json::{Value, json};
    use std::env;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const CONFIG: &str = ".lint-action.json";
    const BASELINE: &str = ".lint-action-baseline.json";
    const UNUSUAL: &str = "new,:%\nname.ts";
    const DUPLICATION: &str = "error source-duplication: 10 shared tokens (minimum 10, blocking) at copy.tsx:1:19-1:45 and original.ts:1:23-1:49";

    fn checked(output: Output) -> Vec<u8> {
        assert!(output.status.success(), "{output:?}");
        output.stdout
    }

    fn git(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(checked(output))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn setup_commit(repository: &Path) {
        git(repository, &["config", "user.name", "Fixture"]);
        git(
            repository,
            &["config", "user.email", "fixture@example.invalid"],
        );
        git(repository, &["config", "commit.gpgsign", "false"]);
    }

    fn commit(repository: &Path, message: &str) -> String {
        git(repository, &["add", "--", "."]);
        git(repository, &["commit", "-qm", message]);
        git(repository, &["rev-parse", "HEAD"])
    }

    fn build(workspace: &Path, tools: &tempfile::TempDir) -> String {
        let passing = workspace.join("action-fixture-pass");
        let failing = workspace.join("action-fixture-fail");
        fs::create_dir(&passing).unwrap();
        git(&passing, &["init", "-q"]);
        setup_commit(&passing);
        fs::write(
            passing.join(CONFIG),
            json!({"mode": "baseline", "limits": {"typescript": 1},
            "cognitive_limits": {"typescript": 1}, "duplication": {"minimum_tokens": 10}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            passing.join("legacy.ts"),
            "export const old = () => true ? 1 : 0;\n",
        )
        .unwrap();
        fs::write(
            passing.join("original.ts"),
            "function original() { prepare(item); send(item); }\n",
        )
        .unwrap();
        let generate = |root: &Path| {
            let output = common::command(tools)
                .args(["baseline", "--root"])
                .arg(root)
                .output()
                .unwrap();
            fs::write(root.join(BASELINE), checked(output)).unwrap();
        };
        generate(&passing);
        let revision = commit(&passing, "Trusted policy and existing debt");
        git(
            workspace,
            &[
                "clone",
                "-q",
                "--no-hardlinks",
                "--",
                "action-fixture-pass",
                "action-fixture-fail",
            ],
        );
        setup_commit(&failing);
        fs::write(
            failing.join(UNUSUAL),
            "export const fresh = () => false || true;\n",
        )
        .unwrap();
        fs::write(
            failing.join("nested.ts"),
            "function nested() { if (a) { if (b) work(); } }\n",
        )
        .unwrap();
        fs::write(
            failing.join("copy.tsx"),
            "function copy() { prepare(item); send(item); }\n",
        )
        .unwrap();
        generate(&failing);
        commit(
            &failing,
            "Candidate adds a violation and waives it in its baseline",
        );
        revision
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn verify(passing: &Path, failing: &Path) {
        let summarize = |report: &Value| -> Vec<(String, String, String)> {
            assert_eq!(report["mode"], "baseline");
            report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    let path = item["path"]
                        .as_str()
                        .or_else(|| item["occurrences"][0]["path"].as_str())
                        .unwrap();
                    (
                        path.to_owned(),
                        item["rule"].as_str().unwrap().to_owned(),
                        item["status"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        };
        let rows = |rows: &[(&str, &str, &str)]| -> Vec<_> {
            rows.iter()
                .map(|(path, rule, status)| {
                    (path.to_string(), rule.to_string(), status.to_string())
                })
                .collect()
        };
        let legacy = ("legacy.ts", "cyclomatic-complexity", "baselined");
        assert_eq!(summarize(&read(passing)), rows(&[legacy]));
        let failing = read(failing);
        assert_eq!(
            summarize(&failing),
            rows(&[
                ("copy.tsx", "source-duplication", "blocking"),
                legacy,
                ("nested.ts", "cyclomatic-complexity", "blocking"),
                ("nested.ts", "cognitive-complexity", "blocking"),
                (UNUSUAL, "cyclomatic-complexity", "blocking"),
            ])
        );
        let occurrences = &failing["findings"][0]["occurrences"];
        assert_eq!(occurrences[0]["path"], "copy.tsx");
        assert_eq!(occurrences[1]["path"], "original.ts");
    }

    struct Fixture {
        directory: tempfile::TempDir,
        tools: tempfile::TempDir,
        workspace: PathBuf,
        revision: String,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let workspace = directory.path().join("workspace");
            fs::create_dir(&workspace).unwrap();
            let tools = common::without_python();
            let revision = build(&workspace, &tools);
            Self {
                directory,
                tools,
                workspace,
                revision,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.directory.path().join(name)
        }

        fn command(&self, workspace: &Path, repository: &str) -> Command {
            let mut command = common::command(&self.tools);
            command.arg("github-scan").envs([
                ("LINT_GATE_REPOSITORY", repository.as_ref()),
                ("LINT_GATE_PR_BASE", self.revision.as_ref()),
                ("LINT_GATE_POLICY_REF", OsStr::new("")),
                ("LINT_GATE_POLICY_DIRECTORY", OsStr::new("")),
                ("GITHUB_EVENT_NAME", OsStr::new("pull_request")),
                ("GITHUB_WORKSPACE", workspace.as_os_str()),
                ("RUNNER_TEMP", self.directory.path().as_os_str()),
                ("GITHUB_OUTPUT", self.path("output").as_os_str()),
                ("GITHUB_STEP_SUMMARY", self.path("summary").as_os_str()),
                ("GITHUB_REPOSITORY", OsStr::new("example/repo")),
                ("GITHUB_SERVER_URL", OsStr::new("https://github.com")),
            ]);
            command
        }

        fn run(&self, command: &mut Command, code: i32) -> (String, String, Option<PathBuf>) {
            fs::write(self.path("output"), "").unwrap();
            fs::write(self.path("summary"), "").unwrap();
            let result = command.output().unwrap();
            let annotation = String::from_utf8(result.stdout).unwrap();
            assert_eq!(
                result.status.code(),
                Some(code),
                "{annotation}\n{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output = fs::read_to_string(self.path("output")).unwrap();
            let summary = fs::read_to_string(self.path("summary")).unwrap();
            let path = output
                .lines()
                .find_map(|line| line.strip_prefix("report="))
                .map(PathBuf::from);
            if path.is_some() {
                assert!(output.lines().any(|line| line == format!("outcome={code}")));
            }
            (annotation, summary, path)
        }

        fn normalize(&self, report: &Path) -> Value {
            let command = common::command(&self.tools)
                .arg("github-app-report")
                .env("SCAN_REPORT", report)
                .env("RUNNER_TEMP", self.directory.path())
                .env("GITHUB_RUN_ID", "100")
                .env("GITHUB_RUN_ATTEMPT", "1")
                .env("GITHUB_SHA", &self.revision)
                .output()
                .unwrap();
            checked(command);
            let directory = self.path("lint-gate-app");
            let report = read(&directory.join("report.json"));
            fs::remove_dir_all(directory).unwrap();
            report
        }
    }

    fn trusted_policy_and_output() {
        let fixture = Fixture::new();
        let (annotations, summary, passing) = fixture.run(
            &mut fixture.command(&fixture.workspace, "action-fixture-pass"),
            0,
        );
        assert!(
            annotations.contains(
                "::warning title=lint-gate quality gate,file=action-fixture-pass/legacy.ts"
            )
        );
        assert!(summary.contains("Outcome: **passed**"));
        let mut command = fixture.command(&fixture.workspace, "action-fixture-fail");
        let (annotations, summary, failing) = fixture.run(&mut command, 1);
        let failing = failing.unwrap();
        let passing = passing.unwrap();
        verify(&passing, &failing);
        for text in [
            format!("file=action-fixture-fail/copy.tsx,line=1,col=19::{DUPLICATION}"),
            "file=action-fixture-fail/new%2C%3A%25%0Aname.ts,line=1,col=22::fresh has error cyclomatic complexity 2 (limit 1, blocking)".to_owned(),
            "nested has error cognitive complexity 3 (limit 1, blocking)".to_owned(),
        ] { assert!(annotations.contains(&text), "{annotations}"); }
        for text in [
            "Outcome: **violations**",
            "Baselined: 1 · Blocking: 4",
            "#### New (4)",
            "Unchanged findings: **1**",
            "source-duplication 10 (minimum 10, blocking",
            "copy.tsx:1:19-1:45",
            "original.ts:1:23-1:49",
        ] {
            assert!(summary.contains(text), "{summary}");
        }

        for (path, counts) in [
            (
                &passing,
                json!({"blocking": 0, "reported": 0, "baselined": 1}),
            ),
            (
                &failing,
                json!({"blocking": 4, "reported": 0, "baselined": 1}),
            ),
        ] {
            let report = fixture.normalize(path);
            assert_eq!(report["version"], 1);
            assert_eq!(report["run_id"], 100);
            assert_eq!(report["attempt"], 1);
            assert_eq!(report["revision"], fixture.revision);
            assert_eq!(report["counts"], counts);
            if path == &failing {
                assert!(
                    report["samples"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|sample| sample.as_str().unwrap().contains(UNUSUAL))
                );
            }
        }

        let repository = fixture.workspace.join("action-fixture-fail");
        assert_eq!(
            read(&repository.join(BASELINE))["duplication_entries"][0]["score"],
            10
        );
        fs::write(repository.join(CONFIG), r#"{"mode":"report-only","limits":{"typescript":999},"severities":{"cyclomatic-complexity":"pass","source-duplication":"pass"}}"#).unwrap();
        let (annotations, _, dirty) = fixture.run(&mut command, 1);
        assert!(annotations.contains("complexity 2 (limit 1, blocking)"));
        assert!(annotations.contains(DUPLICATION));
        let dirty = read(&dirty.unwrap());
        assert!(dirty["comparison"]["candidate_revision"].is_null());
        assert_eq!(
            dirty["comparison"]["changes"],
            read(&failing)["comparison"]["changes"]
        );
        git(&repository, &["restore", "--", CONFIG]);

        git(&repository, &["rm", "--cached", "--", BASELINE]);
        git(
            &repository,
            &["commit", "-qm", "Trusted revision has no baseline"],
        );
        command.env(
            "LINT_GATE_PR_BASE",
            git(&repository, &["rev-parse", "HEAD"]),
        );
        let (annotations, _, _) = fixture.run(&mut command, 2);
        assert!(annotations.contains("invalid_baseline"));

        let directory = fixture.path("trusted");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(CONFIG), r#"{"mode":"strict","limits":{"typescript":1},"cognitive_limits":{"typescript":1},"severities":{"cyclomatic-complexity":"pass","cognitive-complexity":"warn"}}"#).unwrap();
        command.env("LINT_GATE_POLICY_DIRECTORY", &directory);
        let (annotations, summary, report) = fixture.run(&mut command, 0);
        assert_eq!(read(&report.unwrap())["mode"], "strict");
        for text in [
            "::warning title=lint-gate quality gate,file=action-fixture-fail/nested.ts",
            "nested has warn cognitive complexity 3 (limit 1, reported)",
            "Largest contributions: +2 if at line 1",
        ] {
            assert!(annotations.contains(text), "{annotations}");
        }
        assert!(summary.contains("Warning severity: 1 · Error severity: 0"));
        let optional_baseline = directory.join(BASELINE);
        std::os::unix::fs::symlink("unused.json", &optional_baseline).unwrap();
        fixture.run(&mut command, 0);
        fs::remove_file(&optional_baseline).unwrap();
        std::os::unix::fs::symlink(repository.join("missing.json"), &optional_baseline).unwrap();
        assert!(
            fixture
                .run(&mut command, 2)
                .0
                .contains("outside GITHUB_WORKSPACE")
        );

        command.env("LINT_GATE_POLICY_DIRECTORY", repository);
        assert!(
            fixture
                .run(&mut command, 2)
                .0
                .contains("outside GITHUB_WORKSPACE")
        );
    }

    fn exact_event_base_and_synthetic_merge() {
        let fixture = Fixture::new();
        let repository = fixture.workspace.join("action-fixture-fail");
        let head = git(&repository, &["rev-parse", "HEAD"]);
        git(
            &repository,
            &["checkout", "-q", "--detach", &fixture.revision],
        );
        fs::write(
            repository.join(".gitattributes"),
            "legacy.ts export-ignore\n",
        )
        .unwrap();
        fs::write(
            repository.join("base-only.ts"),
            "export const extra = () => false ? 2 : 3;\n",
        )
        .unwrap();
        let scores = |worse, better| {
            for (name, expression) in [("worse", worse), ("better", better)] {
                fs::write(
                    repository.join(format!("{name}.ts")),
                    format!("const {name} = () => {expression};\n"),
                )
                .unwrap();
            }
        };
        scores("a || b", "a || b || c");
        let base = commit(
            &repository,
            "Advance the PR base separately from the policy ref",
        );
        git(
            &repository,
            &["merge", "-q", "--no-ff", "--no-commit", &head],
        );
        scores("a || b || c", "a || b");
        let merge = commit(&repository, "Synthetic merge");
        let mut command = fixture.command(&repository, ".");
        command
            .env("LINT_GATE_PR_BASE", &base)
            .env("LINT_GATE_POLICY_REF", &fixture.revision);
        let (_, summary, report) = fixture.run(&mut command, 1);
        let mut report = read(&report.unwrap());
        let comparison = report
            .as_object_mut()
            .unwrap()
            .remove("comparison")
            .unwrap();
        assert_eq!(comparison["base_revision"], base);
        assert_eq!(comparison["candidate_revision"], merge);
        assert_eq!(comparison["unchanged"], 2);
        let categories: Vec<_> = comparison["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["category"].as_str().unwrap())
            .collect();
        assert_eq!(
            categories,
            ["new", "new", "new", "new", "worsened", "improved"]
        );
        assert!(summary.find("#### New").unwrap() < summary.find("#### Worsened").unwrap());
        assert!(summary.find("#### Worsened").unwrap() < summary.find("#### Improved").unwrap());
        for text in [
            "cyclomatic-complexity 2 → 3 (limit 1, blocking)",
            "cyclomatic-complexity 3 → 2 (limit 1, blocking)",
        ] {
            assert!(summary.contains(text));
        }
        for path in ["copy.tsx", "original.ts", "new%2C%3A%25%0Aname.ts"] {
            assert!(
                summary.contains(&format!(
                    "https://github.com/example/repo/blob/{merge}/{path}#L1"
                )),
                "{summary}"
            );
        }
        assert!(!summary.contains("base-only.ts"));

        command.env("LINT_GATE_PR_BASE", "1".repeat(40));
        let (_, summary, unavailable) = fixture.run(&mut command, 1);
        let mut unavailable = read(&unavailable.unwrap());
        let comparison = unavailable
            .as_object_mut()
            .unwrap()
            .remove("comparison")
            .unwrap();
        assert_eq!(report, unavailable);
        assert_eq!(comparison["status"], "unavailable");
        assert_eq!(comparison["reason"], "base_root_unavailable");
        assert!(summary.contains("Comparison unavailable") && summary.contains("fetch-depth: 0"));

        git(&repository, &["checkout", "-q", "--detach", &base]);
        fs::write(repository.join("bad.ts"), "function (").unwrap();
        command.env(
            "LINT_GATE_PR_BASE",
            commit(&repository, "Historical base contains a parse error"),
        );
        git(&repository, &["checkout", "-q", "--detach", &merge]);
        let (_, summary, failed) = fixture.run(&mut command, 1);
        assert_eq!(
            read(&failed.unwrap())["comparison"]["reason"],
            "base_analysis_failed"
        );
        assert!(summary.contains("bad.ts") && summary.contains("parse_error"));

        fs::write(repository.join("bad.ts"), "function (").unwrap();
        let (_, summary, failed) = fixture.run(&mut command, 2);
        assert_eq!(
            read(&failed.unwrap())["comparison"]["reason"],
            "candidate_analysis_failed"
        );
        assert!(summary.contains("Outcome: **analysis failed**"));

        command.env("GITHUB_EVENT_NAME", "pull_request_target");
        assert!(
            fixture
                .run(&mut command, 2)
                .0
                .contains("use the pull_request event")
        );
    }

    fn stage_verified_scan_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.workspace.join("action-fixture-fail");
        let head = git(&candidate, &["rev-parse", "HEAD"]);
        fs::write(
            candidate.join(UNUSUAL),
            "export const fresh = () => true || false;",
        )
        .unwrap();
        let revision = commit(&candidate, "Synthetic tree changes one finding location");
        // A dirty copy of the policy must not waive findings in the independent scan.
        fs::write(candidate.join(CONFIG), r#"{"mode":"report-only"}"#).unwrap();
        let tool = common::binary().canonicalize().unwrap();
        let mut rescan = common::command(&fixture.tools);
        rescan
            .args(["github-rescan"])
            .arg(tool)
            .env("GITHUB_WORKSPACE", &fixture.workspace)
            .env("LINT_GATE_REPOSITORY", "action-fixture-fail")
            .env("VERIFIED_BASE", &fixture.revision)
            .env("VERIFIED_REVISION", &revision)
            .env("RUNNER_TEMP", fixture.directory.path())
            .env("GITHUB_OUTPUT", fixture.path("rescan"));
        let output = rescan.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let outputs = fs::read_to_string(fixture.path("rescan")).unwrap();
        let report = PathBuf::from(
            outputs
                .lines()
                .find_map(|line| line.strip_prefix("report="))
                .unwrap(),
        );
        assert_eq!(read(&report)["mode"], "baseline");
        assert!(outputs.contains("outcome=1"));

        let output = common::command(&fixture.tools)
            .arg("github-stage-evidence")
            .env("GITHUB_WORKSPACE", &fixture.workspace)
            .env("LINT_GATE_REPOSITORY", "action-fixture-fail")
            .env("VERIFIED_BASE", &fixture.revision)
            .env("VERIFIED_REVISION", &revision)
            .env("VERIFIED_HEAD", &head)
            .env("SOURCE_RUN", "99")
            .env("SOURCE_ATTEMPT", "1")
            .env("GITHUB_RUN_ID", "123")
            .env("GITHUB_RUN_ATTEMPT", "2")
            .env("SCAN_REPORT", &report)
            .env("RUNNER_TEMP", fixture.directory.path())
            .env("GITHUB_OUTPUT", fixture.path("stage"))
            .output()
            .unwrap();
        checked(output);
        let evidence = fixture.path("lint-gate-findings");
        assert_eq!(
            fs::read(evidence.join("report.json")).unwrap(),
            fs::read(report).unwrap()
        );
        let manifest = read(&evidence.join("anchors.json"));
        assert_eq!(
            manifest["artifact_name"],
            "lint-gate-verified-findings-123-2"
        );
        assert_eq!(
            manifest["compatible_paths"],
            json!(["copy.tsx", "legacy.ts", "nested.ts", "original.ts"])
        );
        assert_eq!(
            fs::read_to_string(fixture.path("stage")).unwrap(),
            "artifact_name=lint-gate-verified-findings-123-2\n"
        );
    }

    pub fn main() {
        let args: Vec<_> = env::args_os().skip(1).collect();
        match args.as_slice() {
            [] => {
                trusted_policy_and_output();
                exact_event_base_and_synthetic_merge();
                stage_verified_scan_evidence();
            }
            [mode, workspace] if mode == "build" => {
                println!("{}", build(Path::new(workspace), &common::without_python()))
            }
            [mode, passing, failing] if mode == "verify" => {
                verify(Path::new(passing), Path::new(failing))
            }
            _ => panic!("expected no arguments, build <workspace>, or verify <passing> <failing>"),
        }
    }
}

fn main() {
    #[cfg(unix)]
    unix::main();
}
