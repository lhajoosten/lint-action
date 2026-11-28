# Exits from finally blocks

Rule ID `finally-exit`. Supported languages are Python, TypeScript and TSX.

A transfer out of cleanup can discard an exception or replace a pending return. For example, this Python function returns `None` even when `load()` raises an exception.

```python
def read():
    try:
        return load()
    finally:
        return None
```

The rule reports each `return`, `break` or `continue` whose target lies outside an enclosing `finally` block. A jump to an outer loop qualifies. A break from a loop entirely inside cleanup is allowed, as is a return from a function defined inside cleanup. TypeScript labels and switch targets are considered. In Python, a jump from a loop’s `else` suite targets an enclosing loop. A single transfer crossing nested cleanup blocks produces one finding. Explicit `raise` and `throw` are outside the rule’s scope.

## Adoption

```json
{
  "mode": "strict",
  "finally_exit": true,
  "severities": {"finally-exit": "warn"}
}
```

This enables the rule alongside the default complexity checks. Use `"limits": {}` to run this rule alone. All three supported languages participate within the policy’s include and exclude paths. A warning is advisory. Set severity to `error` to block in strict or baseline mode. Baselines record scored debt only. Report-only mode stays advisory, and `pass` suppresses the rule. Choose one enforcement owner if another linter already checks this pattern. ESLint also covers `throw` and Ruff covers Python. [ESLint no-unsafe-finally](https://eslint.org/docs/latest/rules/no-unsafe-finally) · [Ruff B012](https://docs.astral.sh/ruff/rules/jump-statement-in-finally/)

## Reports and comparisons

The finding includes the rule, language, path, scope, name, exact statement range, transfer kind, explanation, severity and disposition. The range uses one-based UTF-8 byte columns and an exclusive end. There is no numeric threshold. Enabling the rule opts into scan report format 3 and comparison format 2.

Comparison counts an exit as unchanged when its transfer, enclosing identity and exact range match. Relocating a statement can yield a removed and a new occurrence. Neither comparison nor baseline generation grants an exception to an unsafe transfer.
