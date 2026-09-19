# pylint-rs

A fast Python linter written in Rust, inspired by [Astral](https://astral.sh)'s
[ruff](https://github.com/astral-sh/ruff).

Parses Python source with [`rustpython-parser`](https://github.com/RustPython/Parser)
and walks the AST looking for common issues.

## Rules

| Code | Description |
| ---- | ----------- |
| `F401` | Imported name is never used |
| `B006` | Mutable (`list`/`dict`/`set`) default argument |
| `E711` | Comparison to `None` using `==`/`!=` instead of `is`/`is not` |
| `E722` | Bare `except:` clause |
| `E501` | Line too long (> 100 characters) |

## Usage

```sh
cargo run -- path/to/file_or_directory
```

With no arguments, it lints the current directory recursively.

Exit codes: `0` no issues, `1` issues found, `2` a file failed to parse/read.

## Building

```sh
cargo build --release
```

## License

MIT
