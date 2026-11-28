# lint-gate

<img width="2172" height="724" alt="image" src="https://github.com/user-attachments/assets/ce155939-1d71-4150-8afa-b5ba263b1e34" />

A Tree-sitter CLI and composite GitHub Action for cyclomatic complexity in Rust, TypeScript, TSX, and Python, opt-in callable body line limits in those languages, and cognitive complexity and opt-in source duplication in TypeScript and TSX. The CLI and Action use the same parser, scoring contract, and policy.

## Run locally

Build the CLI with Rust 1.88 or newer and a C compiler. The resulting binary bundles the grammars and scans source directly.

```sh
cargo install --path . --locked
lint-gate scan --root /path/to/project
```

For a concise local report, use `lint-gate scan --root /path/to/project --format text`. JSON remains the default; both formats use the same exit codes. For example:

```text
src/example.ts:4:1: blocking cyclomatic-complexity: score 14, limit 12 (example)
blocked: 1 file(s) scanned, 0 skipped
```

Create `.lint-action.json` at the source root. This example reports findings without enforcing the limits.

```json
{
  "mode": "report-only",
  "include": ["src/**", "scripts/**"],
  "exclude": ["**/generated/**", "**/node_modules/**"]
}
```

Optionally set `"callable_line_limits": {"rust": 50, "typescript": 50, "tsx": 50, "python": 50}` to report long callable bodies; this map independently selects languages and has no default. See the [counting contract](docs/rules/callable-lines.md) before choosing limits.

Both complexity rules default to a limit of **12**. Omitting `limits` enables cyclomatic complexity in all supported languages. If supplied, `limits` selects the enabled languages and gives their cyclomatic limits. Cognitive complexity runs for enabled TypeScript and TSX. Set `cognitive_limits` to override either cognitive limit independently.

Set `severities` to override enforcement per stable rule ID. `pass` disables the rule, `warn` reports findings without blocking or using baseline exceptions, and `error` follows the configured mode. Missing entries retain the existing mode behavior, and severity does not enable a rule that is otherwise inactive.

```json
{
  "mode": "strict",
  "limits": {"typescript": 10},
  "severities": {"cyclomatic-complexity": "error", "cognitive-complexity": "warn"}
}
```

```json
{
  "mode": "report-only",
  "limits": {"typescript": 10, "tsx": 12},
  "cognitive_limits": {"typescript": 15}
}
```

TypeScript uses limits 10 and 15 in this example. TSX uses 12 for both rules. Selectable extensions are `.rs`, `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, and `.pyi`. Complexity limits must be positive integers, and a score equal to its complexity limit passes. `include` defaults to all paths and `exclude` defaults to empty. Globs are relative to the source root and use slash separators. Exclusions take precedence. The scanner prunes excluded directories and Git metadata, skips symlinks and unselected files it encounters, and reads up to 4 MiB per regular UTF-8 source file. A selection must include at least one source file.

To compare TypeScript and TSX for source duplication, add `"duplication": {}` to the policy. It uses a minimum of 50 shared syntax tokens by default. See the [source duplication rule](docs/rules/source-duplication.md) for matching and rollout details.

Set `"finally_exit": true` to report `return`, `break`, or `continue` when the statement leaves a `finally` block in TypeScript, TSX or Python. Start with `"severities": {"finally-exit": "warn"}` to review existing findings. Each finding points at the transfer and explains which pending outcome it can discard. [Finally exit rule](docs/rules/finally-exit.md)

Use `--config /path/to/policy.json` to select another policy. To scan specific files or directories within the configured selection, pass their root-relative paths after `--`.

## Adopt the gate

| Mode | Behavior for qualifying findings |
| --- | --- |
| `report-only` | Reports the finding and allows the run to pass |
| `strict` | Blocks the run |
| `baseline` | Allows an existing exception when its identity is unique and its score is at or below the recorded score |

Generate a baseline explicitly after reviewing the report. A successful generation writes JSON to stdout. Keep the checked-in baseline intact if generation fails.

Baseline generation records new exceptions for error rules, including in report-only mode. It carries forward existing exceptions for rules set to `warn` or `pass` from the baseline beside the policy file.

```sh
lint-gate baseline > .lint-action-baseline.json.new &&
  mv .lint-action-baseline.json.new .lint-action-baseline.json
```

Set `mode` to `baseline` and check the policy and baseline into the base branch. The CLI reads `.lint-action-baseline.json` beside the policy unless you pass `--baseline`. A complexity baseline entry records the rule ID, path, language, and qualified callable identity without line numbers. Entries from earlier versions that omit the rule ID apply to cyclomatic complexity. Moving to another path or named scope, changing the callable identity, or introducing ambiguous names prevents a waiver. A directly returned callable uses a return-site identity within its enclosing scope. Other unbound anonymous callables are reported and cannot inherit waivers. Only unique, over-limit callables with stable identities are included when generating a baseline. Remove stale entries during deliberate baseline review.

After an accepted finding improves, preview safe reductions with `lint-gate baseline-review --root /path/to/project`. It requires the existing baseline and a complete successful scan under the selected policy. The command writes a deterministic RFC 6902 JSON Patch to stdout, leaves the baseline file untouched, and explains preserved entries on stderr. An empty array (`[]`) means there are no safe changes. Review and apply the patch explicitly before committing the baseline. The command preserves new, worsened, inactive, excluded, or ambiguous exceptions, along with exceptions for rules set to `warn` or `pass`.

Use `lint-gate validate --root /path/to/project` to check candidate policy and its baseline separately from enforcement.

## Add to a workflow

For an independent check on GitHub-hosted Ubuntu, put this caller in `.github/workflows/source-complexity.yml`. Replace the reference with a reviewed full commit SHA containing the reusable workflow. Choose the triggers for your repository and check in `.lint-action.json` with `"mode": "report-only"` for advisory onboarding.

```yaml
name: Source complexity

on:
  pull_request:
    branches: [main]
  push:
    branches: [main]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  complexity:
    uses: lhajoosten/lint-gate/.github/workflows/source-complexity.yml@<reviewed-full-commit-sha>
```

The reusable workflow has read-only repository access, checks out the candidate without persisting credentials, and uses the PR base policy. The first PR that adds the policy can bootstrap it without scanning against a missing base policy. A push or dispatch checks the selected revision. Disable **Source complexity** in Actions to pause this caller.

To add an advisory check under your own GitHub App identity, see [GitHub App check setup](docs/github-app-check.md).

To integrate the composite Action directly with an existing job, add it after checkout. Pin it to a reviewed full commit SHA. The analyzer uses no repository credentials or project build. The Action builds the pinned analyzer and grammars using Cargo.

```yaml
permissions:
  contents: read

# Steps in an existing job
steps:
  - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
    with:
      fetch-depth: 0
      persist-credentials: false
  - uses: lhajoosten/lint-gate@<reviewed-full-commit-sha>
    id: complexity
```

For pull requests, including forks, the Action scans the checkout with policy from the event's base SHA. Full checkout history makes that commit available locally. Land the policy before enabling the step. A trusted caller can set `policy-ref` to a full commit SHA already present in the checkout, or set `policy-directory` to an absolute directory outside `GITHUB_WORKSPACE` containing the trusted policy files. Keep these inputs under workflow maintainer control. Use `repository` if the checkout is in a subdirectory of `GITHUB_WORKSPACE`.

The Action emits up to ten GitHub annotations and a job summary under **lint-gate quality gate**. Its `report` output is the absolute path to the JSON report on the runner. For private repository sharing, enable access for eligible consumer repositories in Actions settings. [GitHub private Action access](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository#managing-access-for-a-private-repository)

### Compare a pull request locally

Materialize the exact PR base and the candidate checked out in CI. Set `CANDIDATE_SHA` to the PR merge commit when reproducing the reusable workflow.

```bash
git worktree add --detach /path/to/base "$BASE_SHA"
git worktree add --detach /path/to/candidate "$CANDIDATE_SHA"
lint-gate compare --base-root /path/to/base --root /path/to/candidate \
  --base-revision "$BASE_SHA" --candidate-revision "$CANDIDATE_SHA"
```

Comparison uses the policy and baseline from the base directory for both scans. Use `--config` and optionally `--baseline` to reproduce a trusted policy override. The optional revision arguments label the trees you provide. The command scans the full policy-selected workspace, including unchanged code that can be part of a duplicate.

The Actions summary shows new and worsened findings before improvements, fixes, and removals. It counts unchanged findings and displays both locations of each duplicate. The full candidate report remains available through the Action's `report` output. The composite Action labels a clean candidate checkout with its HEAD SHA. A checkout with local changes still uses the working tree for the gate.

## Report contract

`scan` writes JSON with `format_version: 2`, `scoring_version`, `mode`, file counts, `findings`, and `diagnostics`. Scored findings include a stable rule ID, severity, score, limit, and disposition (`reported`, `baselined`, or `blocking`). Locations use one-based lines and UTF-8 byte columns, with an exclusive end. Findings are sorted by the first occurrence's path and start position, then by rule. Complexity findings retain the source path, language, callable name, scope, and range. Cognitive findings include the scoring contributions. Duplication findings carry two locations in `occurrences`, and `limit` is an inclusive minimum. Diagnostics have distinct types for configuration, baseline, path, source reads, encoding, file size, parser errors, empty scans, and the duplication analysis budget.

`compare` adds a `comparison` object with its own `format_version: 1`, the base and candidate revisions when supplied, and a coverage status. A complete comparison includes an `unchanged` count and an ordered `changes` list. Each change has a category, match status, rule, limit, and previous and current observations when available. Observations include scores, locations, and enforcement disposition. A passing score has a null disposition. A callable crossing the limit is `new`, a callable returning to the limit or below is `fixed`, and a finding without a trusted counterpart is listed as `new` or `removed`. Ambiguous entries are marked and sorted after other entries within their category. Changes between two passing scores are omitted.

Enabling `finally_exit` selects report format 3 and comparison format 2. Existing scored findings retain their fields. A `finally-exit` finding carries the statement location, transfer kind and explanation. Its finding and comparison observations omit numeric measurements. Repositories using the GitHub App should upgrade their publisher before enabling the rule in the scanner policy.

When analysis is incomplete, the comparison reports `unavailable` with a reason and any base diagnostics. With an explicit trusted `--config`, you can omit `--base-root` and still receive the full candidate report. The exit code always reflects the candidate gate.

| Exit | Meaning |
| --- | --- |
| 0 | Passed, with possible report-only or baselined findings |
| 1 | Blocking findings |
| 2 | Analysis or invocation failure, including in report-only mode |

See the [cyclomatic scoring contract](docs/rules/cyclomatic-complexity.md), the [cognitive scoring contract](docs/rules/cognitive-complexity.md), and the [Release Log pilot](docs/release-log-pilot.md).

## Rolling Release

Configure `RELEASE_APP_ID` and `RELEASE_BOT_USER_ID` as Actions variables and `RELEASE_APP_PRIVATE_KEY` as an Actions secret. The existing `RELEASE_LOG_PRIVATE_KEY` secret is accepted as a fallback. The caller pins one Rolling Release workflow revision for the read-only preview and separate release writer. Set `RELEASE_LOG_WRITE_ENABLED=false` to pause merge writes. Manual dispatch on `main` runs a read-only preview.

The **Release Log metadata** workflow checks ordinary PR titles, release context, and authored commits against the same release policy. It reads current PR metadata through GitHub with read-only permissions and reruns after title edits. After observing a successful check, require **`release-log / metadata`** from **GitHub Actions** on `main` in branch protection or a ruleset; preserve existing required checks such as **`quality`**. Merge queues need a separate check on `merge_group`.

To verify a release tag and publish the composite Action, see [GitHub Marketplace releases](docs/marketplace-release.md).

## Maintain the rules

Language mappings live in `src/grammar.rs`. An adapter maps a grammar to callable scopes, named containers, and typed cyclomatic decision points. TypeScript cognitive scoring lives in `src/cognitive.rs`. `src/analyzer.rs` owns tree traversal and decision attribution, while `src/model.rs` defines common diagnostics and locations. Keep new rule behavior independent of CLI and Action output. Add a source fixture with hand-calculated scores and assert the observable result through the CLI. Review the rule reference and scoring version when changing the contract.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release
```
