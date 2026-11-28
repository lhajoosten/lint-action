# Callable body lines

`callable-lines` is an opt-in, per-language count of physical code lines in each callable body. Set `callable_line_limits` independently of `limits`; omitted languages are disabled. Scores above the configured positive integer are findings; equality passes. The rule supports Rust, TypeScript, TSX and Python, and uses the existing severity, baseline and comparison behavior.

The parser identifies the same callables and identities as complexity. Only body tokens count. Each physical line is counted once per callable when it contains a non-comment token other than `{` or `}`. This gives the following contract:

| Example | Count | Reason |
| --- | ---: | --- |
| `function f(` followed by parameters and `) {` | 0 | Signature is outside the body |
| A line with only `{` or `}` | 0 | Braces alone are excluded |
| A blank line or comment-only line | 0 | Neither contains a code token |
| `a(); b();` | 1 | Multiple statements on one line count once |
| `return (` / `value` / `);` on three lines | 3 | A multiline statement contributes each code line |
| A multiline string or template literal | One per nonblank literal line | Literal content is code, even if it looks like a comment |
| A Python docstring | One per nonblank literal line | A docstring is a body expression |
| `() => value` or `lambda: value` | 1 | Expression bodies count their expression |
| A nested function | Its body counts for the nested callable | Its declaration and body are excluded from the enclosing callable; other outer tokens on the same lines still count |

Blank lines within literals are excluded. Closing delimiters of literals count as code. Comments alongside code do not add another line. Structural punctuation other than braces counts; this includes closing parentheses on a separate line. Decorators and callable signatures are outside the callable's body. The enclosing callable may count tokens surrounding a nested callable, such as an assignment operator or a semicolon.

This is a size signal, not a substitute for complexity or design review. Declarative TSX and multiline data literals can score highly without difficult control flow. Start with report-only, review examples in each language, and choose limits for the repository before enabling enforcement.

## Report-only rehearsal

The mixed-language Action fixture used a two-line limit for each language. It reported the three-line routines in Rust, TypeScript and Python and the four-line TSX view. The Action exited successfully. In a real workflow, a routine with many sequential stages like the Rust example can benefit from extracting a named step. The TSX view is declarative markup, so its report alone gives little reason to split it.
