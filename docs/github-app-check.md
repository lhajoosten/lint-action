# Publish Lint Gate checks with a GitHub App

The optional publisher adds **Lint Gate (advisory)** under your App's identity. Keep the existing Actions analysis job in your ruleset as the required check. The App check links to the exact Actions run attempt and reports clean scans, warnings, blocking policy findings, incomplete analysis, and policy bootstrap. The advisory check uses a neutral conclusion for warnings, blocking findings, and incomplete results.

## Set up the App

1. Create a GitHub App with repository **Checks: Read and write**. An incoming webhook is unnecessary for this workflow. Install the App on the repository that receives the pull requests.
2. Set the repository Actions variable `LINT_GATE_APP_ID` to the numeric App ID and the Actions secret `LINT_GATE_APP_PRIVATE_KEY` to the App's PEM private key.
3. Enable `publish-report: true` on the existing read-only scan call. Use a reviewed full commit SHA that includes this option. Keep the job IDs and workflow names you already require in your ruleset.
4. Add the publisher caller below to the default branch and set `LINT_GATE_APP_PUBLISH` to `true`. The publisher uses the private key to mint a short-lived token for the current repository with Checks write. The scan needs only Contents read and runs on fork PRs without App credentials.

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
      app-id: ${{ vars.LINT_GATE_APP_ID }}
      source-workflow: source-complexity.yml
      scanner-workflow: lhajoosten/lint-gate/.github/workflows/source-complexity.yml@<scanner-sha>
      analysis-job: complexity / complexity
    secrets:
      app-private-key: ${{ secrets.LINT_GATE_APP_PRIVATE_KEY }}
```

Set `LINT_GATE_APP_PUBLISH` to `false` or remove it to pause publication. Turn off `publish-report` to pause report uploads as well. GitHub retains these small report artifacts for seven days.

## Revision and failure behavior

The scan uses GitHub's synthetic merge tree with policy from the event's base commit. The publisher verifies the source workflow and attempt, the job's recorded candidate, and the current PR's synthetic merge and its parents before creating a check on the PR head. It skips publication if a push, rebase, or base update makes the candidate impossible to associate. The Checks API can also decline to attach a check for a fork PR. Use the native Actions run for feedback in either case.

The publisher uses only the trusted workflow revision's code. It reads the report ZIP as bounded data and uses Actions step conclusions to distinguish blocking findings from analysis failures. Missing, expired, and malformed artifacts are reported in the advisory output. A success with unavailable details receives a neutral result. The publishing workflow reports configuration and API failures independently of the scan.

The repository serializes check writes in the publisher job. Each App owns one check keyed by repository, check name, and PR head. A repeat delivery reuses the existing result. The publisher orders updates by analysis workflow run number and attempt, so an earlier run cannot replace a later one.

## Verify the lint-gate pilot

The pilot pins the reusable scanner to the same reviewed commit SHA in both caller workflows. Update the two pins together when upgrading the scanner. After the workflow is available on the default branch and the App is installed, verify the following on GitHub:

- Open one branch PR and one fork PR. Confirm App attribution, check visibility, and the link to the exact attempt. Compare the commit displayed by the check to the PR head and the synthetic merge SHA in the summary to the source run.
- Rerun all jobs for one PR. Confirm the existing check updates to the new attempt without duplicating.
- Exercise a policy violation and confirm that the native Actions job still controls the required check. Check a run with publishing disabled or a missing App ID to confirm that the analysis outcome is independent of publication.
