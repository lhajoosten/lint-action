# Cognitive complexity

Rule ID `cognitive-complexity`, scoring version 1. The rule covers TypeScript and TSX. Each callable starts at zero and owns its contributions, including those inside nested control flow. A nested callable starts fresh and has its own report.

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
