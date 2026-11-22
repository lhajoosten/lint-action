# GitHub Marketplace releases

Rolling Release creates the version tag and GitHub release. **Verify Marketplace release** runs when a stable release is published and can also be dispatched with an existing tag. It checks that the repository is public and the tagged version matches Cargo, then builds the tagged code and runs the CLI and Action fixtures. The job summary links to the release editor.

After verification, open that link, select **Publish this Action to the GitHub Marketplace**, and save the release. For the first listing, the repository owner must accept the Marketplace Developer Agreement. GitHub requires two-factor authentication to publish. The Marketplace listing is for the root composite Action, which consumers can use as `lhajoosten/lint-gate@vX.Y.Z` with the selected version. [GitHub Marketplace publishing](https://docs.github.com/en/actions/how-tos/create-and-publish-actions/publish-in-github-marketplace)

To verify an older release or rerun verification after correcting its configuration, open **Actions → Verify Marketplace release → Run workflow** and enter the existing stable tag. The verification workflow uses read-only access. The release editor is where you publish the listing.
