# Standalone Tree-sitter lint bot and GitHub Action

## Problem Statement

I want a lint bot that I can add to my existing GitHub workflows and use across Rust, TypeScript, and Python projects. Today I can turn on individual linter rules, but I still need to configure, run, and interpret each ecosystem’s checks separately. I want a SonarQube-style pull request quality gate with actionable findings, informed by the best rules from Oxlint, ESLint, SonarQube, and Ruff.

Cyclomatic complexity is the first rule. Release Log is a practical first consumer. A read-only Oxlint inventory scanned 83 files under its source directory and reported 41 functions above Oxlint’s default maximum of 20. A rollout needs to catch regressions without requiring a bulk cleanup before adoption. Release Log also has a separate maintainability concern around repeated regex-based validation and untyped strings flowing past input boundaries.

## Solution

Build a standalone lint project with a local CLI and a GitHub Action that can be added as a single step to an existing job. Parse source with Tree-sitter. Ship one cyclomatic complexity rule for Rust, TypeScript, TSX, and Python, with a documented scoring contract and actionable file-level annotations. Let maintainers inspect existing findings, adopt a baseline, and fail CI on new or worsened violations. Use the same analyzer and policy locally and in CI.

Design for a small, curated rule catalog. Take rule ideas and test cases from established linters, and document the semantics and supported languages for each shipped rule. A contributor should be able to add a language adapter or an independently tested rule through a defined interface. The first release validates that design with one rule and three language families.

## User Stories

1. As a maintainer, I want to install one Action in an existing job, so that I can add a quality gate without restructuring CI.
2. As a maintainer, I want the same Action to scan mixed-language workspaces, so that a monorepo can use one policy.
3. As a Rust contributor, I want complexity findings for functions, methods, and closures, so that I can locate decision-heavy code.
4. As a TypeScript contributor, I want TypeScript and TSX support, so that application and UI code are checked.
5. As a Python contributor, I want complexity findings for regular and async functions, methods, and lambdas, so that the check fits ordinary Python code.
6. As a contributor, I want a local command with the same output and exit status as CI, so that I can reproduce failures.
7. As a PR author, I want the finding on the relevant file and line with the callable name, score, and limit, so that I know what to inspect.
8. As a reviewer, I want a run summary with files scanned, findings, and analysis errors, so that I can judge coverage and the gate outcome.
9. As a maintainer, I want configurable maximum scores by language and path selection, so that I can apply a policy appropriate to each repository.
10. As a maintainer, I want a report-only mode, so that I can see the effect of a policy before making it required.
11. As a maintainer of an existing codebase, I want to record existing over-limit functions, so that I can adopt a gate without a bulk refactor.
12. As a reviewer, I want new over-limit functions and worsening exceptions to fail the check, so that the baseline does not become a blanket waiver.
13. As a maintainer, I want gate policy and baseline changes to take effect from a trusted revision in pull request CI, so that a change cannot grant itself an exception.
14. As a contributor from a fork, I want the same read-only check, so that my changes receive feedback without repository secrets.
15. As a CI operator, I want invalid configuration, unreadable source, and parser errors to produce distinct diagnostics, so that an incomplete scan cannot look clean.
16. As a tool integrator, I want versioned machine-readable findings with deterministic ordering, so that another CI system can consume them.
17. As a project maintainer, I want the analyzer and grammars pinned together, so that scores change only when I review a tool update.
18. As a linter user, I want a rule reference with equivalents and differences across Oxlint, ESLint, SonarQube, and Ruff, so that I can choose which check owns enforcement.
19. As a rule author, I want reusable typed diagnostics and fixture conventions, so that adding a rule reuses the CLI and Action integration.
20. As a language adapter author, I want a defined way to map grammar nodes to the rule’s concepts, so that I can extend support without adding CI-specific behavior.
21. As a Release Log maintainer, I want the standalone Action to fit the existing Quality workflow, so that I can pilot the tool without coupling it to the release CLI.
22. As a Release Log maintainer, I want repeated parsing and validation to move toward shared typed values in a separate change, so that release contracts become easier to follow while the linter remains reusable.

## Implementation Decisions

- Give the analyzer its own repository and versioning. Start with a Rust CLI using pinned Tree-sitter grammars and a composite GitHub Action that invokes the same CLI with structured arguments. Document SHA pinning, initial runner support, and local execution. Bundle the grammars with the analyzer. Depend on source files and policy data at analysis time. The Action can run on a pre-checked-out workspace and requires only read access for source inspection.
- Use Tree-sitter syntax nodes as the source of findings. Map grammar-specific nodes into typed callable and branch categories. Keep scoring, settings, diagnostic formatting, and CI policy independent of grammar adapters. Use stable rule IDs and typed diagnostics. Extend the catalog through reviewed built-in rules and adapters.
- Publish an explicit scoring table with worked examples before enforcing the first rule. Start each callable at one and add documented decision points. Count branches and loops, short-circuit operators, and applicable language constructs per that table. Cover Rust match arms, guards and early exits, TypeScript switch cases, optional chaining and nullish operators, and Python match cases, exception handlers and comprehensions. Specify inclusion or exclusion and the exact increment for each construct. Score nested callables independently. Report the boundary for syntax inside unexpanded Rust macros and other constructs the parser cannot reliably inspect.
- Use one documented scoring version. Include the version with machine output and baselines. Compare representative fixtures against Oxlint and ESLint complexity, SonarQube’s documented metric, and Ruff C901. Record deliberate differences and reference the upstream rule when adding a rule. [Oxlint complexity](https://oxc.rs/docs/guide/usage/linter/rules/eslint/complexity) · [ESLint complexity](https://eslint.org/docs/latest/rules/complexity) · [SonarQube metrics](https://docs.sonarsource.com/sonarqube-server/2026.1/user-guide/code-metrics/metrics-definition) · [Ruff McCabe settings](https://docs.astral.sh/ruff/settings/#lint_mccabe_max-complexity)
- Validate repository policy as data, including enabled languages, path inclusions and exclusions, per-language limits, and enforcement mode. Analyze regular files within the selected root with bounded reads. Ignore unselected extensions and report skipped files. Surface invalid configuration, unreadable files, Tree-sitter error or missing nodes, and unexpectedly empty scans as analysis errors. Fail the run on analysis errors in either mode. [Tree-sitter error and missing nodes](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)
- Support strict enforcement and optional checked-in baseline enforcement. A score equal to the limit passes. In baseline mode, an over-limit callable passes only if its uniquely matching baseline entry records an equal or higher score. Identify entries by repository-relative path, language, and stable callable identity, independent of line numbers. An unmatched, relocated, or ambiguously matched callable receives no inherited exception. Generate baselines explicitly and reject incompatible scoring versions. Report baselined and blocking findings separately.
- In pull request CI, scan the checked-out candidate source using the configuration and baseline from the trusted base revision, or explicit trusted caller inputs. Allow candidate policy to be checked separately for validity. Treat source paths and names as data when producing GitHub annotations. Emit annotations and a job summary with a stable check name and limits on displayed findings. Keep the complete machine-readable report available as CLI output. Distinguish successful runs, violations, and analysis failures with documented exit statuses. [GitHub Action reuse options](https://docs.github.com/en/actions/concepts/workflows-and-actions/reusing-workflow-configurations)
- Pilot in Release Log’s existing Quality workflow, plus Rust and Python fixtures. Observe report-only output, review thresholds and baseline, then make the new gate enforceable in the consuming workflow. Establish one owner for complexity enforcement per language during adoption. Oxlint can provide a local TypeScript reference while the standalone scoring policy is evaluated.
- Keep Release Log’s regex cleanup as a separate consumer follow-up. Prioritize repeated validation of full Git object IDs, digests, stable versions and tags. Reuse shared validated representations while keeping each domain’s accepted inputs explicit. For version 1 release context, reuse the existing Markdown and YAML parsers and use explicit fence handling where it improves clarity. Preserve conventional policy, PR context errors, accepted inputs, snapshot replay, released changelog bytes, and package/CLI behavior. Keep concise, isolated lexical checks where they clearly express the format.

## Testing Decisions

- Make the packaged CLI the primary seam. Run it on hand-scored Rust, TypeScript, TSX, Python, and mixed-language fixtures. Assert scores, callable ranges, diagnostics, report format, and exit statuses. Cover the threshold boundary, nested scopes, anonymous callables, multiline syntax, Unicode locations, and branch-like text in strings and comments.
- Test policy using the same CLI. Cover empty selections, invalid config, unsupported file extensions, excluded paths, malformed syntax, unreadable source, and paths outside the scan root. Verify deterministic results with reordered inputs. Exercise unchanged baseline debt, improvement, regression, new violations, moved and ambiguous identities, and incompatible scoring revisions.
- Use one GitHub Actions fixture to exercise the published Action end to end against a checkout. Verify a passing run, a new violation with an inline annotation, a legacy violation recorded in the summary, and a candidate edit to the baseline that cannot waive its own finding. Verify correct handling of unusual file names in report and annotation output.
- Use Release Log’s existing Quality workflow and architecture check as integration prior art. If the parser cleanup proceeds, use its release-context, offline snapshot replay, changelog, and packaged CLI fixtures to assert observable results and byte stability. Run the canonical repository verification for any consumer changes.

## Out of Scope

- Full SonarQube feature coverage, hosted dashboards, a custom GitHub App, automated fixes, and direct PR comments.
- Importing third-party linter configs, emulating each linter’s full rule engine, or promising score parity across tools.
- Additional lint rules, language families, runtime-loaded plugins, compiler-backed type resolution, and macro expansion in the first release.
- A blanket regex ban or any Release Log runtime dependency on the analyzer.

## Further Notes

The reusable product is the CLI, rule catalog, and GitHub Action. Release Log is an adoption target. Before selecting the analyzer core, compare a thin prototype with Rust Code Analysis against the hand-scored fixtures and adoption requirements. The project can reuse an existing engine if it covers the supported grammars and permits auditable scoring, source ranges, and versioning. [Rust Code Analysis supported languages](https://mozilla.github.io/rust-code-analysis/languages.html)

After the first rollout, evaluate additional rules from the referenced linters on their applicability across languages, actionable diagnostics, and fixture-backed semantics. Prefer a shared rule where the language constructs support the same claim. Publish language-specific coverage as part of each rule’s contract.
