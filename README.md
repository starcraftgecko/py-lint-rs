# pylint-rs

A fast Python linter written in Rust, inspired by [Astral](https://astral.sh)'s
[ruff](https://github.com/astral-sh/ruff).

Parses Python source with [`rustpython-parser`](https://github.com/RustPython/Parser)
and walks the AST looking for common issues.

## Rules

| Code | Description |
| ---- | ----------- |
| `F401` | Imported name is never used (auto-fixable) |
| `B006` | Mutable (`list`/`dict`/`set`) default argument (auto-fixable) |
| `E711` | Comparison to `None` using `==`/`!=` instead of `is`/`is not` (auto-fixable) |
| `E722` | Bare `except:` clause |
| `E501` | Line too long (> 100 characters) |
| `D103` | Public top-level function is missing a docstring (functions named with a leading `_` are ignored) |

## Usage

```sh
cargo run -- path/to/file_or_directory
```

With no arguments, it lints the current directory recursively.

### Auto-fix (prototype: F401, E711, B006 only)

```sh
cargo run -- --fix path/to/file_or_directory
```

Three independent fixers, each narrowly scoped, sharing one diff/apply
engine ([src/fix.rs](src/fix.rs)):

- **F401** - removes fully-unused, single-line `import`/`from ... import ...`
  statements. A `from x import a, b` statement where only `a` is dead, or an
  import that spans multiple lines, is left alone rather than risk an unsafe
  edit.
- **E711** - rewrites `x == None` / `x != None` to `x is None` / `x is not
  None`. Only simple two-operand comparisons are touched (a chained
  comparison like `a == None == b` is refused as ambiguous), and only when
  the operator has whitespace on both sides in the source (rewriting
  `x==None` to `xisNone` would corrupt the file, so that's refused too).
- **B006** - hoists a mutable default into a `None`-guarded assignment:
  `def f(x=[]):` becomes `def f(x=None):` with `if x is None: x = []`
  inserted into the body (after a leading docstring, if any). Refused when
  a function has more than one mutable default (start small - one function
  needing two coordinated insertions is deliberately out of scope for now),
  when the literal default spans multiple lines, or when the insertion
  point shares a physical line with other code (e.g. `def f(x=[]): return
  x` written on one line) - there's no safe text edit for that last case.

Both F401 and E711 are pure deletions/substitutions that fit neatly into a
"start/end span, optional replacement text" model. **B006 does not**: it
needs to *insert* a brand-new statement, which the diff renderer had no
representation for and had to grow a third case to handle (a zero-width
edit whose replacement is itself multiple new lines - see the comment
above `render_diff`). The apply step needed no changes at all; a zero-width
span is already just "remove nothing, insert this." That asymmetry - one
shared function needed no changes, the other needed a real extension - is
the one crack this prototype has found in the "one engine for every fixer"
idea so far.

All fixers print a diff of every change before writing the file, then the
file is re-linted so you see what's left. No other rule has an auto-fix
yet. Each fixer runs its own independent AST pass (see [src/fix.rs](src/fix.rs))
so a bug in one can't corrupt another, and all of them currently operate on
one file at a time.

#### Explainable refusal: the apply engine verifies before it writes

Detection happens against one snapshot of a file; applying happens
(logically) later. `apply_fixes` never assumes those are still the same
file, and never tries to be clever about the gap between them:

- **Stale content is refused, never fuzzy-applied.** Every `Fix` records the
  exact text it expects to find at its target range. Before writing
  anything, `apply_fixes` re-reads that range from the source it's actually
  given and requires an *exact* match - not "close enough." A one-character
  difference is refused exactly like a completely different line would be.
- **Overlapping or colliding fixes are refused, never silently ordered.**
  If two fixes target overlapping ranges, or claim the identical insertion
  point, which one should apply first is genuinely ambiguous - so neither
  does. This is a real, reproducible case, not a hypothetical one: an
  unused import as a function's *first* statement, in a function that also
  needs its mutable default fixed, makes F401's delete and B006's insert
  land on the exact same offset. Before this check existed, that exact
  input crashed the tool outright (`byte range starts at 27 but ends at
  13`) instead of refusing - confirmed by reproducing it against the CLI
  before writing the fix. Guessing the "right" order was possible, but
  guessing is exactly what this project is trying not to do.
- **Every refusal says why**, naming the line, the fixer's own description,
  and (for stale content) the exact expected-vs-found text - so a human or
  another agent can tell what happened without re-deriving it. If any fix
  in a batch is refused, none of them are applied to that file; there is no
  partial-apply mode.

See `RefusalReason` and the tests below `apply_fixes` in
[src/fix.rs](src/fix.rs) for the exact cases covered.

#### Safe auto-fix with automatic rollback

`--fix` alone only does detect/diff/apply. For a full safety net around it -
apply, rerun this repo's test suite, and automatically revert on failure -
use the wrapper script instead:

```sh
pwsh scripts/safe-fix.ps1 -Path examples/bad.py
```

Flow: refuse to run if the target has uncommitted changes (no clean
rollback baseline otherwise) -> build -> `--fix` (detect, diff, apply) ->
`cargo test` -> if tests fail, `git checkout` the target automatically and
exit non-zero; if they pass, leave the fix in place and exit zero.

This lives outside the Rust binary on purpose: "rerun tests" is specific to
whichever project owns the file being fixed. This repo's own `cargo test`
suite (which includes fixture-locking regression tests) is the verification
oracle for its own example files; a real deployment would swap in that
project's test command instead.

Exit codes: `0` no issues, `1` issues found, `2` a file failed to parse/read.

## Building

```sh
cargo build --release
```

## License

MIT
