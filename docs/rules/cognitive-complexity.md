# Cognitive complexity

Rule ID `cognitive-complexity`, scoring version 1. The rule covers TypeScript and TSX by default when those languages are selected. Python requires an explicit `cognitive_limits.python` threshold alongside `limits.python`. Each callable starts at zero and owns its contributions, including those inside nested control flow. A nested callable starts fresh and has its own report.

| Construct | Points |
| --- | --- |
| `if`, `for` (including `in` and `of`), `while`, `do`, `switch`, `catch`, ternary | 1 plus the enclosing control-flow nesting depth |
| `else if`, `else`, labeled `break` or `continue` | 1 |
| Each run of the same short-circuit operator in a connected logical expression | 1 |

Each branch body adds one level for structures inside it. An `else if` chain shares the first `if`'s depth. The bodies of `else if` and `else` add a level. A switch adds one level for all cases. Conditions, ordinary blocks, `try`, and `finally` add no nesting by themselves. Each ternary branch adds a level.

Logical runs follow the written order of `&&`, `||`, and `??`. Parentheses around a logical subexpression are transparent. For example, `a && (b && c) || d` adds 2. An intervening expression such as a call separates its argument's logical chain from the surrounding chain.

Case labels, optional chaining, default values, logical assignments, ordinary returns, throws, and unlabeled jumps add zero. The syntax-only analyzer also leaves recursion unscored.

## Worked scores

| Function shape | Cyclomatic | Cognitive |
| --- | ---: | ---: |
| `if (a) { if (b) work(); }` | 3 | 3 |
| `if (!a) return; if (!b) return; work();` | 3 | 2 |
| `switch` with three cases and a default that each return | 4 | 1 |

The JSON finding includes the construct, range, points, and nesting increment for every contribution. The Action shows up to the three largest contributions in the annotation.

The rule follows Sonar's nesting and flow-break model. Its TypeScript contract counts all three documented short-circuit operators, scores nested callables independently, and requires no type or call-graph resolution. [Sonar Cognitive Complexity white paper](https://www.sonarsource.com/docs/CognitiveComplexity.pdf)

## Python scoring

Each Python callable starts with its own score and depth of zero. Contributions point to the token that introduces the construct.

| Construct | Points | Nesting |
| --- | --- | --- |
| `if`, `for`, `async for`, `while`, `except`, `except*`, `match`, conditional expression | 1 plus the enclosing depth | The body or result branch adds one level. Headers, conditions and selectors stay at the enclosing depth. |
| `elif`, `else` including loop and try alternatives | 1 | The body adds one level. Clauses in a chain share the head's depth. |
| `case` and wildcard patterns | 0 | All cases share the match body depth. A case adds no further level. |
| Pattern guard | 1 | A guard adds no level. Operators and nested expressions in it score normally. |
| Connected `and` and `or` expression | 1 per run | Parentheses are transparent. `not` and intervening expressions break the chain. |
| Comprehension `for` and `if` clauses | 1 plus the enclosing depth and number of earlier `for` clauses | Each `for` adds one level for later clauses and the result expression. A filter adds no level. |
| `try`, `finally`, `with`, ordinary block, `return`, `raise`, `break`, `continue` | 0 | These constructs add no depth. |

The leftmost comprehension iterable is evaluated at the enclosing depth. A nested comprehension starts at the depth of the expression containing it. Its own clauses then establish their own levels.

A Python function that nests `if b` under `if a` has cyclomatic complexity 3 and cognitive complexity 3 (1 + 2). Moving those checks into two sequential guard clauses keeps cyclomatic complexity 3 and reduces cognitive complexity to 2 (1 + 1). A separately named helper nested inside either version starts at depth zero and keeps its own score.

The guarded-match fixture from the packaged CLI has two independently scored callables. In `collect`, the match contributes 1, its guard contributes 1, and two Boolean runs contribute 2. The outer comprehension contributes 2 + 3, and the inner comprehension contributes 3 + 4. The total is 16. The nested `keep` has a generator whose loop and filter contribute 1 + 2 for a total of 3. [Python CLI fixture](../../tests/fixtures/cli/python_match_guards_and_nested_comprehensions.json)
