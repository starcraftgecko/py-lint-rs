# pylint-rs

A fast Python linter written in Rust, inspired by [Astral](https://astral.sh)'s
[ruff](https://github.com/astral-sh/ruff).

Parses Python source with [`rustpython-parser`](https://github.com/RustPython/Parser)
and walks the AST looking for common issues.

## Rules

| Code | Description |
| ---- | ----------- |
| `F401` | Imported name is never used (auto-fixable) |
| `B006` | Mutable (`list`/`dict`/`set`) default argument |
| `E711` | Comparison to `None` using `==`/`!=` instead of `is`/`is not` (auto-fixable) |
| `E722` | Bare `except:` clause |
| `E501` | Line too long (> 100 characters) |
| `D103` | Public top-level function is missing a docstring (functions named with a leading `_` are ignored) |

## Usage

```sh
cargo run -- path/to/file_or_directory
```

With no arguments, it lints the current directory recursively.

### Auto-fix (prototype: F401 and E711 only)

```sh
cargo run -- --fix path/to/file_or_directory
```

Two independent fixers, each narrowly scoped, sharing one diff/apply engine
([src/fix.rs](src/fix.rs)):

- **F401** - removes fully-unused, single-line `import`/`from ... import ...`
  statements. A `from x import a, b` statement where only `a` is dead, or an
  import that spans multiple lines, is left alone rather than risk an unsafe
  edit.
- **E711** - rewrites `x == None` / `x != None` to `x is None` / `x is not
  None`. Only simple two-operand comparisons are touched (a chained
  comparison like `a == None == b` is refused as ambiguous), and only when
  the operator has whitespace on both sides in the source (rewriting
  `x==None` to `xisNone` would corrupt the file, so that's refused too).

Both fixers print a diff of every change before writing the file, then the
file is re-linted so you see what's left. No other rule has an auto-fix yet.
Adding a third fixer means writing one more `find_*` function in
[src/fix.rs](src/fix.rs) - the diff renderer and the apply step are already
generic over any mix of whole-line deletions and in-place substitutions.

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
