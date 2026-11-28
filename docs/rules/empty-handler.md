# Empty exception handlers

Rule ID `empty-handler`. The rule reports TypeScript and TSX `catch` blocks containing no statements, including comment-only blocks. In Python it reports bare `except` and syntactically unqualified `except Exception` whose only statement is `pass` or `...`. It does not resolve names or aliases and excludes `except*`. Narrow exception types, rethrows, retries and other statements are outside its scope. This identifies a syntactic pattern, not proof that an exception reaches the handler.

Enable it with `"empty_handler": true` in `.lint-action.json`. It is advisory by default even in strict mode; set `"severities": {"empty-handler": "error"}` to block in strict or baseline mode. Set severity to `pass` to suppress it. Report-only mode remains advisory; baselines contain scored findings and do not waive this rule. Use `"limits": {}` to run this rule alone.

For an intentional handler, put an actual comment inside that handler's body with a nonblank reason:

```python
try:
    remove_optional_file()
except Exception:
    # lint-gate: allow empty-handler -- cleanup is best effort
    pass
```

```ts
try { cleanup(); } catch (error) {
  // lint-gate: allow empty-handler -- cleanup is best effort
}
```

Python comments must be indented into the suite; header or outside comments do not waive it. A waiver applies only to its own handler. Strings and blank reasons do not waive findings. Prefer a narrow exception type or explicit handling where possible. Ruff's [S110](https://docs.astral.sh/ruff/rules/try-except-pass/) overlaps Python `except`/`pass`; choose one enforcement owner to avoid duplicate reports.

Findings give the handler's full range, form (`empty catch`, `bare except`, or `except Exception`), observed body shape, and explanation. Ranges use one-based UTF-8 byte columns and exclusive ends. Enabling the rule selects scan format 3 and comparison format 2; upgrade the GitHub App publisher before enabling it. Comparison considers the form, body shape, identity and exact range; moving a handler can produce a removed and a new finding.
