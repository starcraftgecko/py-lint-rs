use std::collections::HashSet;

use rustpython_parser::ast::{self as ast, Ranged, StmtImport, StmtImportFrom, Suite, Visitor};
use rustpython_parser::Parse;

use crate::line_index::LineIndex;

/// A single safe, whole-line removal of a dead import statement.
///
/// Scope is deliberately narrow: only statements where *every* bound name
/// is unused are eligible, and only when the statement occupies a single
/// physical line. Partially-unused `from x import a, b` statements and
/// multi-line/parenthesized imports are left alone rather than risk an
/// unsafe text edit.
#[derive(Debug, Clone)]
pub struct ImportFix {
    pub line: usize,
    pub description: String,
    delete_start: usize,
    delete_end: usize,
}

struct FixCollector {
    used_names: HashSet<String>,
    // (stmt_start, stmt_end, bound_names)
    import_stmts: Vec<(usize, usize, Vec<String>)>,
}

impl FixCollector {
    fn new() -> Self {
        Self {
            used_names: HashSet::new(),
            import_stmts: Vec::new(),
        }
    }
}

impl Visitor for FixCollector {
    fn visit_expr_name(&mut self, node: ast::ExprName) {
        self.used_names.insert(node.id.as_str().to_string());
    }

    fn visit_stmt_import(&mut self, node: StmtImport) {
        let names = node
            .names
            .iter()
            .map(|alias| {
                alias
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
                    })
            })
            .collect();
        self.import_stmts.push((
            node.range().start().to_usize(),
            node.range().end().to_usize(),
            names,
        ));
    }

    fn visit_stmt_import_from(&mut self, node: StmtImportFrom) {
        let names: Vec<String> = node
            .names
            .iter()
            .filter(|alias| alias.name.as_str() != "*")
            .map(|alias| {
                alias
                    .asname
                    .as_ref()
                    .map(|n| n.as_str().to_string())
                    .unwrap_or_else(|| alias.name.as_str().to_string())
            })
            .collect();
        if !names.is_empty() {
            self.import_stmts.push((
                node.range().start().to_usize(),
                node.range().end().to_usize(),
                names,
            ));
        }
    }
}

fn line_start_offset(source: &str, offset: usize) -> usize {
    source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_end_offset_inclusive_newline(source: &str, offset: usize) -> usize {
    match source[offset..].find('\n') {
        Some(rel) => offset + rel + 1,
        None => source.len(),
    }
}

/// Finds unused-import statements that are safe to delete outright.
pub fn find_unused_import_fixes(source: &str, line_index: &LineIndex) -> anyhow::Result<Vec<ImportFix>> {
    let suite: Suite = Suite::parse(source, "<fix>").map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut collector = FixCollector::new();
    for stmt in suite {
        collector.visit_stmt(stmt);
    }

    let mut fixes = Vec::new();
    for (start, end, names) in &collector.import_stmts {
        let all_dead = names
            .iter()
            .all(|name| name != "_" && !collector.used_names.contains(name));
        if !all_dead {
            continue;
        }

        let (start_line, _) = line_index.line_col(*start);
        let (end_line, _) = line_index.line_col(*end);
        if start_line != end_line {
            // Multi-line import statement - not safe for this narrow prototype.
            continue;
        }

        fixes.push(ImportFix {
            line: start_line,
            description: format!("remove unused import: {}", names.join(", ")),
            delete_start: line_start_offset(source, *start),
            delete_end: line_end_offset_inclusive_newline(source, *end),
        });
    }

    fixes.sort_by_key(|f| f.delete_start);
    Ok(fixes)
}

/// Renders a minimal diff of the lines that would be removed.
pub fn render_diff(source: &str, fixes: &[ImportFix]) -> String {
    let mut out = String::new();
    for fix in fixes {
        let removed_line = source[fix.delete_start..fix.delete_end].trim_end_matches('\n');
        out.push_str(&format!(
            "- {:>4} | {}  # {}\n",
            fix.line, removed_line, fix.description
        ));
    }
    out
}

/// Applies the fixes to `source`, returning the new file contents.
pub fn apply_fixes(source: &str, fixes: &[ImportFix]) -> String {
    let mut sorted = fixes.to_vec();
    sorted.sort_by_key(|f| f.delete_start);

    let mut result = String::with_capacity(source.len());
    let mut last = 0usize;
    for fix in &sorted {
        result.push_str(&source[last..fix.delete_start]);
        last = fix.delete_end;
    }
    result.push_str(&source[last..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixes(source: &str) -> Vec<ImportFix> {
        let line_index = LineIndex::new(source);
        find_unused_import_fixes(source, &line_index).expect("test source should parse")
    }

    #[test]
    fn removes_unused_plain_import() {
        let source = "import os\nprint(1)\n";
        let f = fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "print(1)\n");
    }

    #[test]
    fn removes_unused_from_import() {
        let source = "from collections import OrderedDict\nprint(1)\n";
        let f = fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "print(1)\n");
    }

    #[test]
    fn leaves_used_import_alone() {
        let source = "import os\nprint(os.getcwd())\n";
        let f = fixes(source);
        assert!(f.is_empty());
        assert_eq!(apply_fixes(source, &f), source);
    }

    #[test]
    fn leaves_partially_used_from_import_alone() {
        // `a` is dead but `b` is used - this is a multi-name statement, so
        // the narrow prototype must not touch it at all.
        let source = "from x import a, b\nprint(b)\n";
        let f = fixes(source);
        assert!(f.is_empty());
    }

    #[test]
    fn leaves_underscore_bound_import_alone() {
        let source = "import os as _\nprint(1)\n";
        let f = fixes(source);
        assert!(f.is_empty());
    }

    #[test]
    fn leaves_multiline_import_alone() {
        let source = "from x import (\n    a,\n    b,\n)\nprint(1)\n";
        let f = fixes(source);
        assert!(f.is_empty(), "multi-line imports must be skipped, got {f:?}");
    }

    #[test]
    fn removes_only_the_dead_statement_among_several() {
        let source = "import os\nimport sys\nprint(sys.path)\n";
        let f = fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "import sys\nprint(sys.path)\n");
    }

    #[test]
    fn no_fixes_on_clean_source() {
        let source = "import os\nprint(os.getcwd())\n";
        assert!(fixes(source).is_empty());
    }

    #[test]
    fn removes_indented_import_preserving_surrounding_lines() {
        // The import lives inside an `if` block; deleting its whole physical
        // line must not disturb the indentation of the lines around it.
        let source = "if True:\n    import os\n    print(1)\n";
        let f = fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "if True:\n    print(1)\n");
    }

    #[test]
    fn removes_multiple_fully_dead_imports_in_one_pass() {
        let source = "import os\nimport json\nprint(1)\n";
        let f = fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(apply_fixes(source, &f), "print(1)\n");
    }

    #[test]
    fn known_limitation_dunder_all_reference_is_not_treated_as_usage() {
        // KNOWN LIMITATION: usage is detected only via AST `Name` nodes.
        // A name mentioned solely as a string in `__all__` (a common
        // re-export convention) is invisible to this check, so the import
        // is (incorrectly) treated as dead here. This test pins down the
        // current behavior so it doesn't change silently; see the
        // "next recommendations" note about excluding `__all__`-referenced
        // names from auto-fix before this prototype is trusted more broadly.
        let source = "import os\n\n__all__ = [\"os\"]\n";
        let f = fixes(source);
        assert_eq!(
            f.len(),
            1,
            "documents that __all__ string references are not recognized as usage yet"
        );
    }
}
