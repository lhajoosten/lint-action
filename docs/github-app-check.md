# Publish Lint Gate checks with a GitHub App

The optional publisher adds **Lint Gate (advisory)** under your App's identity. Require the native Actions analysis job in your ruleset for policy enforcement; the advisory App check is not a substitute. The App check links to the exact Actions run attempt and reports clean scans, warnings, blocking policy findings, incomplete analysis, and policy bootstrap. The advisory check uses a neutral conclusion for warnings and a failure conclusion for blocking findings and incomplete analysis.

## Set up the App

1. Create a GitHub App with repository **Checks: Read and write**. The existing advisory check needs only Checks write. To opt into PR conversations, also grant **Pull requests: Read and write**; GitHub accepts that permission for PR Conversation comments and review threads. An incoming webhook is unnecessary. Install the App on the repository that receives the pull requests.
2. Set the repository Actions variable `LINT_GATE_APP_ID` to the numeric App ID and the Actions secret `LINT_GATE_APP_PRIVATE_KEY` to the App's PEM private key.
3. Pin the read-only scan caller to a reviewed full commit SHA and set `publish-report: true` to preserve the existing advisory check input. When conversation publishing is enabled, a separate trusted job independently rescans the verified merge tree; only that fresh scan evidence can drive review-thread or Conversation writes.
4. Add the publisher caller below to the default branch and set `LINT_GATE_APP_PUBLISH` to `true`. The advisory check always runs independently with a Checks-only App token. Set `LINT_GATE_APP_CONVERSATIONS` to `true` to opt into the independent trusted rescan and PR conversations; that writer uses a separate App token with Pull requests write. Its workflow token remains read-only, within the caller's permission ceiling. The scan needs only read permissions and runs on fork PRs without App credentials.

In the example, the scan caller is named **Source complexity** and lives at `.github/workflows/source-complexity.yml`. Its job ID is `complexity`, which results in the analysis job display name `complexity / complexity`. Use the actual workflow name, filename, and job display name if yours differ. Replace both commit SHA placeholders with the reviewed revisions, and use the same scanner SHA in the scan caller and publisher input.

```yaml
name: Publish Lint Gate advisory check

on:
  workflow_run:
    workflows: [Source complexity]
    types: [completed]

permissions:
  actions: read
  contents: read
  pull-requests: read

jobs:
  advisory:
    uses: lhajoosten/lint-gate/.github/workflows/publish-app-check.yml@<publisher-sha>
    with:
      enabled: ${{ vars.LINT_GATE_APP_PUBLISH == 'true' }}
      publish-conversations: ${{ vars.LINT_GATE_APP_CONVERSATIONS == 'true' }}
      app-id: ${{ vars.LINT_GATE_APP_ID }}
      source-workflow: source-complexity.yml
      scanner-workflow: lhajoosten/lint-gate/.github/workflows/source-complexity.yml@<scanner-sha>
      analysis-job: complexity / complexity
    secrets:
      app-private-key: ${{ secrets.LINT_GATE_APP_PRIVATE_KEY }}
```

Set `LINT_GATE_APP_PUBLISH` to `false` or remove it to pause publication. Leave `LINT_GATE_APP_CONVERSATIONS` unset to keep the existing Checks-only advisory behavior. The pilot scanner is pinned to the reviewed #34 severity policy revision in both caller workflows; update both pins together when upgrading. For the pilot, require the native `complexity / complexity` analysis job in branch protection or a ruleset. The publisher does not alter repository protection settings.

## Revision and failure behavior

The scan uses GitHub's synthetic merge tree with policy from the event's base commit. The publisher verifies the source workflow and attempt, the job's recorded candidate, and the current PR's synthetic merge and its parents before creating a check on the PR head. It skips publication if a push, rebase, or base update makes the candidate impossible to associate. The Checks API can also decline to attach a check for a fork PR. Use the native Actions run for feedback in either case.

The conversation publisher first verifies the completed source workflow, attempt, scanner revision, current PR head, base, and synthetic merge commit. A separate job then checks out that merge tree and runs the scanner from the exact verified scanner revision with policy from the trusted base commit. Candidate files are only read as scanner input; no candidate code is run. That read-only job uploads its scan result and source coordinate manifest to the writer job in the same workflow run. The App writer does not check out candidate source. It validates the current PR association before each write and tracks feedback order in its maintained Conversation comment. GitHub does not provide an atomic compare-and-swap across PR metadata and comment writes, so a PR update after the final read can still race with a mutation. A conversation publishing failure does not stop the separate advisory check. The source run's artifact is not used as evidence for conversations.

Blocking findings with a current diff line become App-owned line threads. Findings in changed files without a usable line anchor use file-level threads when GitHub accepts them. Other errors, and all reported warnings, appear in one maintained top-level Conversation comment. Baselined findings do not create required conversations. A complete verified rescan resolves only this App's obsolete finding threads; incomplete analysis and uncertain association leave existing feedback untouched. Review comments include a link to the verified scan attempt.

Conversation routing follows the scan disposition: `blocking` findings become error threads, `reported` findings stay in the warning summary, and `baselined` findings create no conversation. A rule configured with `pass` is absent from scan findings. This remains compatible with reports that predate the per-rule `severity` field.

The native Actions scan remains the policy gate. A manually resolved review thread does not change the native check result. The publisher reports missing App permissions and API failures separately from analysis.

The repository serializes writes by PR. Each App owns one advisory check keyed by repository, check name, and PR head, one warning summary, and at most one active thread per blocking finding identity. Duplicate deliveries update existing feedback. Earlier scans are rejected when their merge no longer matches the current PR.

## Verify the lint-gate pilot

The pilot pins the reusable scanner to the same reviewed commit SHA in both caller workflows. Update the two pins together when upgrading the scanner. After the workflow is available on the default branch and the App is installed, verify the following on GitHub:

- Open one branch PR and one fork PR. Confirm App attribution, check visibility, thread anchoring, and the link to the exact attempt. Compare the commit displayed by the check to the PR head and the synthetic merge SHA in the summary to the source run.
- Rerun all jobs for one PR. Confirm the existing check updates to the new attempt without duplicating.
- Exercise a policy violation and confirm that the native Actions job still controls the required check. Check a run with publishing disabled or a missing App ID to confirm that the analysis outcome is independent of publication.
- On a pilot branch, require the native Actions analysis job and enable GitHub's conversation-resolution requirement. Confirm unresolved blocking threads participate in merge protection, warnings do not, and manually resolving an active blocking thread does not pass the native Actions gate.

## Upgrade and rollback

When upgrading to the Rust-based GitHub integration, select a reviewed
provider revision that contains the reusable scanner and publisher. Keep the
scanner caller's ref and the publisher's expected scanner ref aligned. The
Rust integration uses the existing report artifact version, step names, and
workflow inputs, so existing policy, baselines, check names, and App
permissions carry forward.

Verify the pilot with the existing App installation before promoting the
provider ref. Record the exact scanner and publisher revisions used. To roll
back, restore the previous scanner ref in both callers and select the
previous publisher revision if it changed. Consumers that pin a full SHA can
adopt or roll back by updating those pins together.
