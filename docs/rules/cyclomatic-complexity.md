# Cyclomatic complexity

Rule ID `cyclomatic-complexity`, scoring version 1. Every callable starts at **1**. Each construct below adds **1 per occurrence**. The innermost callable owns its decisions, including those in its signature or default values. Module-level decisions are outside callable scoring.

| Language | Callable boundaries | Decision points |
| --- | --- | --- |
| Rust | Function and method definitions, closures | `if` (including `else if` and `if let`), `for`, `while` (including `while let`), `loop`, `let … else`, `&&`, `\|\|`, each `match` arm (including the wildcard), each match guard, and each postfix `?` |
| TypeScript and TSX | Function and generator declarations and expressions, methods and accessors, arrow functions | `if` (including `else if`), `for` (including `in` and `of`), `while`, `do`, `catch`, each non-default `case`, ternaries, each optional chaining hop, `&&`, `\|\|`, `??` and their assignment forms, and default parameter or destructuring initializers |
| Python | Function and method definitions (including `async def`), lambdas | `if`, `elif`, `for` and `while` (including `async for`), ternaries, `and`, `or`, each `except` or `except*` handler, each `case` (including the wildcard), each match guard, and each comprehension or generator-expression `for` and `if` clause |

Plain `else`, switch default, Python `try`/`else`/`finally`, and unconditional `return`, `break`, `continue`, `raise`, and `throw` add zero. Rust `?` and `let … else` represent conditional exits. Pattern alternatives, JSX tags, non-null assertions, and text in strings or comments add zero. The analyzer sees Rust macro token trees without expansion. It scores syntax only and performs no name resolution, reachability analysis, or TypeScript type checking.

## Worked scores

| Fixture | Callable | Calculation | Score |
| --- | --- | --- | --- |
| Rust `flow.rs` | `sample` | Start + question mark + if + short circuit + three arms + guard | 8 |
| Rust `flow.rs` | `nested` | Start + if | 2 |
| TypeScript `flow.ts` | `sample` | Start + default parameter + case + optional hop + nullish operator | 5 |
| TypeScript `flow.ts` | `nested` | Start + ternary | 2 |
| TSX `view.tsx` | `View` | Start + short circuit + ternary | 3 |
| Python `flow.py` | `sample` | Start + two cases + guard + comprehension for + comprehension if + short circuit + handler | 8 |
| Python `flow.py` | `nested` | Start + ternary | 2 |

## Reference rules

- Oxlint and ESLint use a starting score of one. On the TypeScript and TSX fixtures, both reported 5 for the outer function, 2 for the nested arrow, and 3 for the TSX arrow. The TypeScript sample exercises classic switch scoring, default parameters, optional chains and nullish choices. ESLint also defines separate scopes for class field initializers and static blocks. This rule scores callable scopes. [Oxlint complexity](https://oxc.rs/docs/guide/usage/linter/rules/eslint/complexity) · [ESLint complexity](https://eslint.org/docs/latest/rules/complexity)
- SonarQube complexity is a language-specific metric. Applying its documented JS/TS increments to the outer TypeScript function gives 2 for the callable and the case. This rule also counts the optional hop, default parameter and nullish choice. SonarQube also lists explicit `throw` as an increment. [SonarQube metric definitions](https://docs.sonarsource.com/sonarqube-server/2026.1/user-guide/code-metrics/metrics-definition)
- Ruff C901 derives from McCabe and reported 3 for the Python sample. The Python fixture also counts Boolean expressions, comprehension clauses, the guard, and the lambda as its own callable. [Ruff C901](https://docs.astral.sh/ruff/rules/complex-structure)

## Analyzer selection

A prototype using Rust Code Analysis 0.0.25 found a per-function score of 8 for the Rust sample, 2 for the TypeScript sample, and 3 for the TSX arrow. A smaller fixture containing just one optional hop and a nullish fallback scored 1 in both TypeScript and TSX. Its function spaces provide line ranges, while this gate requires precise columns and explicit control of decision semantics. The analyzer uses pinned Tree-sitter grammars directly for those requirements. [Rust Code Analysis metrics API](https://docs.rs/rust-code-analysis/0.0.25/rust_code_analysis/fn.metrics.html)
