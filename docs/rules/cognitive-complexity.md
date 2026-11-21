# Cognitive complexity

Rule ID `cognitive-complexity`, scoring version 1. The rule covers Rust, Python, TypeScript, and TSX. The default limit is 12 for every enabled language. Each callable starts at zero and owns its contributions, including those inside nested control flow. A nested callable starts fresh and has its own report.

## TypeScript and TSX

| Construct | Points |
| --- | --- |
| `if`, `for` (including `in` and `of`), `while`, `do`, `switch`, `catch`, ternary | 1 plus the enclosing control-flow nesting depth |
| `else if`, `else`, labeled `break` or `continue` | 1 |
| Each run of the same short-circuit operator in a connected logical expression | 1 |

Each branch body adds one level for structures inside it. An `else if` chain shares the first `if`'s depth. The bodies of `else if` and `else` add a level. A switch adds one level for all cases. Conditions, ordinary blocks, `try`, and `finally` add no nesting by themselves. Each ternary branch adds a level.

Logical runs follow the written order of `&&`, `||`, and `??`. Parentheses around a logical subexpression are transparent. For example, `a && (b && c) || d` adds 2. An intervening expression such as a call separates its argument's logical chain from the surrounding chain.

Case labels, optional chaining, default values, logical assignments, ordinary returns, throws, and unlabeled jumps add zero. The syntax-only analyzer also leaves recursion unscored.

## Rust

`if`, `for`, `while`, `loop`, `match`, guarded match patterns, and `let … else` add one point plus their enclosing control-flow depth. Branch and loop bodies add a nesting level, including the fallback block of `let … else`. Match arms share the match body's depth. `else if`, `else`, labeled jumps, and error propagation with `?` each add one point. Logical expressions and let chains add one point per run of `&&` or `||`.

For example, an `if` inside a `for` adds 2 points. The enclosing loop adds 1. A guard in a top-level `match` also adds 2 points, on top of the match's 1 point.

## Python

`if`, `for` (including `async for`), `while`, `match`, case guards, `except` (including `except*`), conditional expressions, and comprehension `for` and `if` clauses each add one point plus their enclosing control-flow depth. `elif` and `else` each add one point. Branch and loop bodies, exception handler bodies, conditional expression branches, and comprehension result expressions add a nesting level. Cases share the match body's depth. `try`, `finally`, and `with` add no nesting on their own. Logical expressions add one point per run of `and` or `or`.

For example, `[item for item in items if valid(item)]` adds 2 points at the top level of a callable. A conditional expression used as its result adds 2 more.

## TypeScript worked scores

| Function shape | Cyclomatic | Cognitive |
| --- | ---: | ---: |
| `if (a) { if (b) work(); }` | 3 | 3 |
| `if (!a) return; if (!b) return; work();` | 3 | 2 |
| `switch` with three cases and a default that each return | 4 | 1 |

The JSON finding includes the construct, range, points, and nesting increment for every contribution. The Action shows up to the three largest contributions in the annotation.

The rule follows Sonar's nesting and flow-break model. The contracts score nested callables independently and use syntax alone across all supported languages. [Sonar Cognitive Complexity white paper](https://www.sonarsource.com/docs/CognitiveComplexity.pdf)
