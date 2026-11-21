# Source duplication

The `source-duplication` rule reports repeated syntax token sequences in TypeScript and TSX. Enable it by adding a `duplication` object to the policy. An empty object uses the default minimum of 50 tokens. Set `minimum_tokens` to a positive integer to override it. A match at the minimum qualifies.

```json
{
  "mode": "report-only",
  "limits": {},
  "duplication": {"minimum_tokens": 50},
  "include": ["src/**"],
  "exclude": ["**/generated/**"]
}
```

This policy runs duplication alone. To run it alongside complexity, supply the desired complexity limits or omit `limits` for the default complexity policy. Duplication selects `.ts`, `.mts`, `.cts`, and `.tsx` independently of the complexity limits and obeys the same path filters. The Action compares the full selected checkout, so a new file can match unchanged source. An explicit CLI path selection limits both sides of a comparison.

## Matching and locations

The rule searches inside brace-delimited TypeScript and TSX statement blocks. These include function, method, block-bodied arrow, control-flow, namespace, and standalone blocks. An outermost statement block is searched once, including any nested syntax inside it. Its opening and closing braces are excluded. Module-level statement lists, class bodies, and expression-bodied arrows do not add block boundaries of their own. Identical syntax can match across the two grammars.

The analyzer compares contiguous Tree-sitter terminal tokens. It ignores comments and whitespace between tokens, and matches identifiers, literal spellings, punctuation, and operators exactly. Whitespace inside a literal or JSX text is part of that token. Compare these bodies at a minimum of 10 tokens.

```ts
function original() {
  prepare(item);
  send(item);
}

function copy() {
  prepare( item ); // Formatting and comments leave the shared tokens unchanged.
  send(item);
}
```

This produces one 10-token finding. Changing an identifier or literal in the shared sequence breaks exact matching at that token. Occurrences can be in one file or different files and cannot share tokens. The matcher grows qualifying runs to their longest non-overlapping extent and consolidates shorter alignments that overlap both ranges of a retained finding.

The JSON report keeps `format_version: 1`. Duplication adds a finding with `rule`, `score`, `limit`, `status`, and exactly two `occurrences`. Each occurrence has a repository-relative `path` and a `range` from the first matched token to the exclusive end of the last one. Columns count one-based UTF-8 bytes. Findings and occurrences have deterministic source order. Annotations and the job summary show both ranges within the existing display limits. JSON contains all findings from a completed scan.

## Rollout and baselines

Start with `report-only`, inspect the pairs, and adjust the minimum for your repository. Use `strict` to block every qualifying pair. To adopt `baseline`, generate and review the baseline with `lint-gate baseline`, then land it together with the policy. On pull requests the Action reads both from the trusted base commit.

A duplication baseline adds optional `duplication_entries` to the existing baseline schema. An entry records the two repository-relative paths and qualified named body identities, the accepted token count, and the exact normalized token sequence. It covers the same pair while its shared sequence is unchanged or shrinks to a contiguous portion of the accepted sequence. Separate paths or named bodies, longer runs, and different token sequences require new exceptions. Unrelated line shifts and comments preserve an exception.

An occurrence needs a unique named body to qualify for an exception. A body with separate reported ranges, repeated scope names, and anonymous or standalone blocks can produce findings that require resolution before baseline enforcement. Multiple pair findings can reference the same occurrence, so a new third copy produces new pairs even when the original pair stays baselined. Existing complexity-only baselines are accepted.

A parse or resource error exits with status 2 in every mode. In particularly repetitive or large selections, `duplication_limit` indicates that the matcher reached its token, candidate, or comparison budget. Reduce the selection or raise the threshold if the budget is reached during matching.

## Release Log sample

A report-only scan of 91 TypeScript source files at Release Log commit `8c4ff5a` demonstrated how the minimum changes the result. All three scans completed with zero diagnostics. [Release Log revision](https://github.com/lhajoosten/release-log/tree/8c4ff5a195dc2b757760d59db1298e4358cbe378)

| Minimum tokens | Reported pairs |
| --- | ---: |
| 35 | 50 |
| 50 | 16 |
| 75 | 2 |

At the default, the largest match shared 99 tokens between `src/approval/history.ts` and `src/version-file-inputs.ts`. Another shared 82 tokens between `src/publication/github-write.ts` and `src/publication/registry.ts`.
