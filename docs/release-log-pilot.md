# Release Log pilot

The Action adapter was run locally against a clean archive of Release Log commit `3929887fbbb8309a55ad819a3d4f0063928a5340`. The selection was `src/**/*.ts` with a TypeScript limit of 20. This matches the threshold used in the earlier Oxlint inventory.

| Run | TypeScript files | Reported | Baselined | Blocking | Analysis errors | Exit |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Report-only | 83 | 41 | 0 | 0 | 0 | 0 |
| Baseline | 83 | 0 | 41 | 0 | 0 | 0 |
| Baseline with a synthetic new function at score 21 | 84 | 0 | 41 | 1 | 0 | 1 |

The synthetic function produced an error annotation with its file, line, callable name, score, and limit. The existing TypeScript debt includes a directly returned arrow in `createTextGenerator`. It received a unique return-site identity within the factory. The regression and generated baseline were tested in temporary directories. [Release Log pilot revision](https://github.com/lhajoosten/release-log/tree/3929887fbbb8309a55ad819a3d4f0063928a5340)

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
