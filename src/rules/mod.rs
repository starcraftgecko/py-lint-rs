use std::collections::HashSet;
use std::path::Path;

use rustpython_parser::ast::{
    self as ast, Constant, Expr, ExprCompare, ExceptHandlerExceptHandler, Ranged,
    StmtAsyncFunctionDef, StmtFunctionDef, StmtImport, StmtImportFrom, Suite, Visitor,
};
use rustpython_parser::Parse;

use crate::diagnostics::Diagnostic;
use crate::line_index::LineIndex;

struct PendingImport {
    bound_name: String,
    line: usize,
    col: usize,
}

struct Checker<'a> {
    file: &'a Path,
    line_index: &'a LineIndex,
    diagnostics: Vec<Diagnostic>,
    imports: Vec<PendingImport>,
    used_names: HashSet<String>,
}

impl<'a> Checker<'a> {
    fn new(file: &'a Path, line_index: &'a LineIndex) -> Self {
        Self {
            file,
            line_index,
            diagnostics: Vec::new(),
            imports: Vec::new(),
            used_names: HashSet::new(),
        }
    }

    fn push(&mut self, offset: usize, code: &'static str, message: String) {
        let (line, col) = self.line_index.line_col(offset);
        self.diagnostics.push(Diagnostic {
            file: self.file.to_path_buf(),
            line,
            col,
            code,
            message,
        });
    }

    fn check_mutable_defaults(&mut self, args: &ast::Arguments) {
        let all = args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.kwonlyargs.iter());
        for arg in all {
            let Some(default) = &arg.default else {
                continue;
            };
            let kind = match default.as_ref() {
                Expr::List(_) => Some("list"),
                Expr::Dict(_) => Some("dict"),
                Expr::Set(_) => Some("set"),
                _ => None,
            };
            if let Some(kind) = kind {
                self.push(
                    arg.def.range().start().to_usize(),
                    "B006",
                    format!(
                        "mutable {} default argument `{}` is shared across calls",
                        kind,
                        arg.def.arg.as_str()
                    ),
                );
            }
        }
    }

    fn is_none_literal(expr: &Expr) -> bool {
        matches!(expr, Expr::Constant(c) if matches!(c.value, Constant::None))
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        for import in &self.imports {
            if import.bound_name == "_" || self.used_names.contains(&import.bound_name) {
                continue;
            }
            self.diagnostics.push(Diagnostic {
                file: self.file.to_path_buf(),
                line: import.line,
                col: import.col,
                code: "F401",
                message: format!("`{}` imported but unused", import.bound_name),
            });
        }
        self.diagnostics
    }
}

impl<'a> Visitor for Checker<'a> {
    fn visit_stmt_import(&mut self, node: StmtImport) {
        for alias in &node.names {
            let bound_name = alias
                .asname
                .as_ref()
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| {
                    alias
                        .name
                        .as_str()
                        .split('.')
                        .next()
                        .unwrap_or(alias.name.as_str())
                        .to_string()
                });
            let (line, col) = self.line_index.line_col(alias.range().start().to_usize());
            self.imports.push(PendingImport {
                bound_name,
                line,
                col,
            });
        }
    }

    fn visit_stmt_import_from(&mut self, node: StmtImportFrom) {
        for alias in &node.names {
            if alias.name.as_str() == "*" {
                continue;
            }
            let bound_name = alias
                .asname
                .as_ref()
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| alias.name.as_str().to_string());
            let (line, col) = self.line_index.line_col(alias.range().start().to_usize());
            self.imports.push(PendingImport {
                bound_name,
                line,
                col,
            });
        }
    }

    fn visit_expr_name(&mut self, node: ast::ExprName) {
        self.used_names.insert(node.id.as_str().to_string());
    }

    fn visit_stmt_function_def(&mut self, node: StmtFunctionDef) {
        self.check_mutable_defaults(&node.args);
        self.generic_visit_stmt_function_def(node);
    }

    fn visit_stmt_async_function_def(&mut self, node: StmtAsyncFunctionDef) {
        self.check_mutable_defaults(&node.args);
        self.generic_visit_stmt_async_function_def(node);
    }

    fn visit_excepthandler_except_handler(&mut self, node: ExceptHandlerExceptHandler) {
        if node.type_.is_none() {
            self.push(
                node.range().start().to_usize(),
                "E722",
                "bare `except:` clause catches all exceptions, including KeyboardInterrupt and SystemExit".to_string(),
            );
        }
        self.generic_visit_excepthandler_except_handler(node);
    }

    fn visit_expr_compare(&mut self, node: ExprCompare) {
        let ops_and_rhs = node.ops.iter().zip(node.comparators.iter());
        let mut offenders = Vec::new();
        let mut prev = node.left.as_ref();
        for (op, rhs) in ops_and_rhs {
            let is_eq = matches!(op, ast::CmpOp::Eq | ast::CmpOp::NotEq);
            if is_eq && (Self::is_none_literal(prev) || Self::is_none_literal(rhs)) {
                offenders.push(node.range().start().to_usize());
            }
            prev = rhs;
        }
        for offset in offenders {
            self.push(
                offset,
                "E711",
                "comparison to `None` should use `is` or `is not`".to_string(),
            );
        }
        self.generic_visit_expr_compare(node);
    }
}

pub fn check(file: &Path, source: &str, line_index: &LineIndex) -> anyhow::Result<Vec<Diagnostic>> {
    let suite: Suite =
        Suite::parse(source, &file.to_string_lossy()).map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut checker = Checker::new(file, line_index);
    for stmt in suite {
        checker.visit_stmt(stmt);
    }
    let mut diagnostics = checker.finish();
    check_line_length(file, source, line_index, &mut diagnostics);
    diagnostics.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
    Ok(diagnostics)
}

const MAX_LINE_LENGTH: usize = 100;

fn check_line_length(
    file: &Path,
    source: &str,
    _line_index: &LineIndex,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (i, line) in source.lines().enumerate() {
        let len = line.chars().count();
        if len > MAX_LINE_LENGTH {
            diagnostics.push(Diagnostic {
                file: file.to_path_buf(),
                line: i + 1,
                col: MAX_LINE_LENGTH + 1,
                code: "E501",
                message: format!("line too long ({len} > {MAX_LINE_LENGTH} characters)"),
            });
        }
    }
}
