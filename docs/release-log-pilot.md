# Release Log pilot

The Action adapter was run locally against a clean archive of Release Log commit `3929887fbbb8309a55ad819a3d4f0063928a5340`. The selection was `src/**/*.ts` with a TypeScript limit of 20. This matches the threshold used in the earlier Oxlint inventory.

| Run | TypeScript files | Reported | Baselined | Blocking | Analysis errors | Exit |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Report-only | 83 | 41 | 0 | 0 | 0 | 0 |
| Baseline | 83 | 0 | 41 | 0 | 0 | 0 |
| Baseline with a synthetic new function at score 21 | 84 | 0 | 41 | 1 | 0 | 1 |

The synthetic function produced an error annotation with its file, line, callable name, score, and limit. The existing TypeScript debt includes a directly returned arrow in `createTextGenerator`. It received a unique return-site identity within the factory. The regression and generated baseline were tested in temporary directories. [Release Log pilot revision](https://github.com/lhajoosten/rolling-release/tree/3929887fbbb8309a55ad819a3d4f0063928a5340)

## Integrate with Quality

Release Log is public. GitHub's cross-repository sharing for private Actions covers eligible private consumers. Hosted adoption requires an Action distribution accessible to the public workflow, pinned by SHA. [GitHub private Action access](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository#managing-access-for-a-private-repository)

1. Land this policy as `.lint-action.json` in Release Log first.

   ```json
   {
     "mode": "report-only",
     "limits": {"typescript": 20},
     "include": ["src/**/*.ts"],
     "exclude": ["**/node_modules/**", "**/.worktrees/**", "**/.pnpm-store/**"]
   }
   ```

2. Add the pinned Action to the existing `verify` job in `.github/workflows/quality.yml`, immediately after checkout and before package setup. The job already fetches full history without persisting credentials.

   ```yaml
   - name: TypeScript complexity
     uses: <accessible-owner>/<action-repository>@<reviewed-full-commit-sha>
   ```

3. Review the report-only findings. Generate and review the baseline at the chosen threshold, then switch the policy to `baseline`. Land both files together. The next PR will use that policy from its base commit. Release Log's Oxlint config currently leaves the complexity rule disabled, so the Action can own complexity enforcement without removing an existing rule.

## Pull request comparison rehearsal

The Action adapter ran locally with the advisory policy from Rolling Release's PR #159 base, `ee6220dc185b507afba454f53c5001ace9375aa3`, and the checked-out two-parent merge, `5031f16f4f35a13a7a7d39b0d8c016061c03590d`. It scanned 91 candidate files and returned the same 188 advisory findings and successful exit as the standalone candidate scan. The comparison counted 168 unchanged findings and surfaced two matched cyclomatic regressions, from 37 to 38 and from 16 to 20, both against a limit of 10. The summary included both source links for duplicate pairs. Ambiguous identities appeared as separate new or removed entries. [Rolling Release PR #159](https://github.com/lhajoosten/rolling-release/pull/159)

## Text-generation provider boundary

Release Log's `text-generation-owns-ai-sdk` contract reserves direct dependencies on `ai` and `@ai-sdk/*` for `src/text-generation.ts`. Production TypeScript imports the project's text-generation API when it needs optional generation. The dependency-cruiser check covers resolved package subpaths, re-exports and type-only imports. The existing transitive planning and deterministic release-notes checks keep those paths offline. [Pilot issue](https://github.com/lhajoosten/lint-gate/issues/7) · [Release Log AI SDK ADR](https://github.com/lhajoosten/rolling-release/blob/14d3113d231d779c57170087ce2c374bf9c0c4b9/docs/adr/0002-ai-sdk-provider-integration.md)

The contract passed on the 112-module production graph through Release Log's architecture verification, which its Quality workflow already runs. Fixtures rejected SDK Core and provider imports outside the wrapper, including a type-only Core import and SDK subpath re-exports. The same fixtures accepted imports inside the wrapper and an application import of the project API. A forbidden edge produces the rule name, importer and resolved dependency in the Quality output.
