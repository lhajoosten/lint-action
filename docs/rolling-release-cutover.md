# Rolling Release v0.10.0 cutover

Lint Gate uses Rolling Release to prepare a release PR, then create a stable tag and GitHub Release at the reviewed landing. The Action is the published artifact. The caller keeps image publication disabled. The [Marketplace verifier and listing steps](marketplace-release.md) follow the published stable release.

## Before enabling writes

1. Confirm there is no open legacy release proposal or release writer in flight. Keep proposal and publication writes disabled during cutover. The new caller requires `RELEASE_LOG_WRITE_ENABLED=true` to write.
2. Set `ROLLING_RELEASE_BOT` to `rolling-release-log[bot]` after confirming the installed release App identity. Keep `RELEASE_APP_ID` and the existing private key secret. The caller accepts either the current `RELEASE_LOG_PRIVATE_KEY` name or `RELEASE_APP_PRIVATE_KEY`. The existing stable tag is the baseline, so no first-release starting SHA is needed.
3. Extend the release App ruleset to reserve writes to `release/next` for the release App. The existing ruleset covers `release-log/main` and `release-log-series/*/main`. Keep strict `quality` protection and preserve the ordinary PR metadata check. Rehearse `proposal-check / proposal` on the App’s protected proposal branch and confirm the displayed name before requiring it.
4. With owner approval, update these previously merged PR titles. Keep the original PR numbers and descriptions.

| PR | Reviewed title |
| --- | --- |
| [#31](https://github.com/lhajoosten/lint-gate/pull/31) | `feat(pr-feedback): publish advisory Lint Gate checks with a GitHub App` |
| [#32](https://github.com/lhajoosten/lint-gate/pull/32) | `ci(release): verify tagged releases for GitHub Marketplace` |
| [#35](https://github.com/lhajoosten/lint-gate/pull/35) | `ci(lint-gate): restore the App pilot with a pinned reusable scanner` |

5. After the caller lands, dispatch **Rolling Release** on `main` with no retry PR while writes are disabled. Compare the preview with the reviewed release boundary, membership and version files. Enable writes only after the preview and App-only branch protections are verified.
6. On the first proposal, review the exact source and changed files, including Cargo’s manifest and lockfile. A human merges the proposal to authorize publication. Check the resulting tag against that landing, inspect the GitHub Release and wait for the Marketplace verifier. A maintainer publishes the listing in the release editor.

The previous release policy could make `ci` changes eligible for a patch. Under v0.10.0 they wait for a releasable change. The metadata check continues validating conventional titles and authored commits for ordinary PRs. A release-context block is optional. The new planner uses conventional PR titles and an optional plain `Required action: ...` line for its generated notes. For a verified same-repository App proposal on `release/next`, the provider checks its generated release record and description.

Pause the writer by setting `RELEASE_LOG_WRITE_ENABLED=false`. For an interrupted publication after the stable tag exists, use the protected `main` dispatch with the reviewed merged proposal number. If the tag is missing, rerun the original promotion. Record the exact provider and landing SHAs when collecting evidence. [Pinned provider setup](https://github.com/lhajoosten/rolling-release/blob/v0.10.0/docs/consumer-setup.md)
