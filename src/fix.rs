use std::collections::HashSet;

use rustpython_parser::ast::{self as ast, Ranged, StmtImport, StmtImportFrom, Suite, Visitor};
use rustpython_parser::Parse;

use crate::line_index::LineIndex;

/// A single safe text edit proposed by one of the fixers below.
///
/// A `Fix` is either a whole-line deletion (`replacement` empty, `start`/`end`
/// span a full physical line including its trailing newline) or an in-place
/// substitution (`start`/`end` span just the token being replaced). Both
/// shapes are applied and diffed through the same two functions - that
/// reuse is the point: adding a new fixer means writing a new `find_*`
/// function, not a new apply/diff engine.
#[derive(Debug, Clone)]
pub struct Fix {
    pub line: usize,
    pub description: String,
    start: usize,
    end: usize,
    replacement: String,
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

// ---------------------------------------------------------------------
// F401 - remove fully-unused, single-line import statements
// ---------------------------------------------------------------------

struct ImportCollector {
    used_names: HashSet<String>,
    // (stmt_start, stmt_end, bound_names)
    import_stmts: Vec<(usize, usize, Vec<String>)>,
}

impl ImportCollector {
    fn new() -> Self {
        Self {
            used_names: HashSet::new(),
            import_stmts: Vec::new(),
        }
    }
}

impl Visitor for ImportCollector {
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

/// Finds unused-import statements that are safe to delete outright.
///
/// Scope is deliberately narrow: only statements where *every* bound name
/// is unused are eligible, and only when the statement occupies a single
/// physical line. Partially-unused `from x import a, b` statements and
/// multi-line/parenthesized imports are left alone rather than risk an
/// unsafe text edit.
pub fn find_unused_import_fixes(source: &str, line_index: &LineIndex) -> anyhow::Result<Vec<Fix>> {
    let suite: Suite = Suite::parse(source, "<fix>").map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut collector = ImportCollector::new();
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

        fixes.push(Fix {
            line: start_line,
            description: format!("remove unused import: {}", names.join(", ")),
            start: line_start_offset(source, *start),
            end: line_end_offset_inclusive_newline(source, *end),
            replacement: String::new(),
        });
    }

    fixes.sort_by_key(|f| f.start);
    Ok(fixes)
}

// ---------------------------------------------------------------------
// E711 - rewrite `== None` / `!= None` to `is None` / `is not None`
// ---------------------------------------------------------------------

fn is_none_literal(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Constant(c) if matches!(c.value, ast::Constant::None))
}

struct NoneComparisonCollector {
    // (search_window_start, search_window_end, is_eq)
    candidates: Vec<(usize, usize, bool)>,
}

impl NoneComparisonCollector {
    fn new() -> Self {
        Self {
            candidates: Vec::new(),
        }
    }
}

impl Visitor for NoneComparisonCollector {
    fn visit_expr_compare(&mut self, node: ast::ExprCompare) {
        // Chained comparisons (`a == None == b`) are out of scope: which
        // operator to rewrite becomes ambiguous to reason about safely.
        if node.ops.len() == 1 {
            let is_eq = matches!(node.ops[0], ast::CmpOp::Eq);
            let is_not_eq = matches!(node.ops[0], ast::CmpOp::NotEq);
            if is_eq || is_not_eq {
                let comparator = &node.comparators[0];
                if is_none_literal(&node.left) || is_none_literal(comparator) {
                    self.candidates.push((
                        node.left.range().end().to_usize(),
                        comparator.range().start().to_usize(),
                        is_eq,
                    ));
                }
            }
        }
        self.generic_visit_expr_compare(node);
    }
}

/// Finds `== None` / `!= None` comparisons safe to rewrite to `is`/`is not`.
///
/// Scope is deliberately narrow: only simple two-operand comparisons are
/// handled (no chained comparisons), and only when the operator is
/// surrounded by whitespace on both sides in the source - rewriting
/// `x==None` to `xisNone` would corrupt the file, so that case is refused
/// rather than guessed at.
pub fn find_none_comparison_fixes(source: &str, line_index: &LineIndex) -> anyhow::Result<Vec<Fix>> {
    let suite: Suite = Suite::parse(source, "<fix>").map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut collector = NoneComparisonCollector::new();
    for stmt in suite {
        collector.visit_stmt(stmt);
    }

    let mut fixes = Vec::new();
    for (search_start, search_end, is_eq) in collector.candidates {
        if search_start >= search_end || search_end > source.len() {
            continue;
        }

        let needle = if is_eq { "==" } else { "!=" };
        let Some(rel) = source[search_start..search_end].find(needle) else {
            // Couldn't unambiguously locate the operator - refuse rather than guess.
            continue;
        };
        let op_start = search_start + rel;
        let op_end = op_start + needle.len();

        let has_space_before = source
            .as_bytes()
            .get(op_start.wrapping_sub(1))
            .is_some_and(|b| b.is_ascii_whitespace());
        let has_space_after = source
            .as_bytes()
            .get(op_end)
            .is_some_and(|b| b.is_ascii_whitespace());
        if !has_space_before || !has_space_after {
            continue;
        }

        let replacement = if is_eq { "is" } else { "is not" };
        let (line, _) = line_index.line_col(op_start);
        fixes.push(Fix {
            line,
            description: format!("replace `{needle}` with `{replacement}` for None comparison"),
            start: op_start,
            end: op_end,
            replacement: replacement.to_string(),
        });
    }

    fixes.sort_by_key(|f| f.start);
    Ok(fixes)
}

// ---------------------------------------------------------------------
// Shared diff/apply engine - identical for every fixer above.
// ---------------------------------------------------------------------

/// Renders a minimal diff of what each fix will change.
pub fn render_diff(source: &str, fixes: &[Fix]) -> String {
    let mut sorted = fixes.to_vec();
    sorted.sort_by_key(|f| f.start);

    let mut out = String::new();
    for fix in &sorted {
        let line_start = line_start_offset(source, fix.start);
        let line_end = line_end_offset_inclusive_newline(source, fix.start);
        let before = &source[line_start..line_end];
        let before_trimmed = before.trim_end_matches('\n');

        let is_whole_line_delete =
            fix.replacement.is_empty() && fix.start == line_start && fix.end == line_end;

        if is_whole_line_delete {
            out.push_str(&format!(
                "- {:>4} | {}  # {}\n",
                fix.line, before_trimmed, fix.description
            ));
        } else {
            let local_start = fix.start - line_start;
            let local_end = fix.end - line_start;
            let mut after = String::with_capacity(before.len());
            after.push_str(&before[..local_start]);
            after.push_str(&fix.replacement);
            after.push_str(&before[local_end..]);
            let after_trimmed = after.trim_end_matches('\n');

            out.push_str(&format!("- {:>4} | {}\n", fix.line, before_trimmed));
            out.push_str(&format!(
                "+ {:>4} | {}  # {}\n",
                fix.line, after_trimmed, fix.description
            ));
        }
    }
    out
}

/// Applies the fixes to `source`, returning the new file contents.
pub fn apply_fixes(source: &str, fixes: &[Fix]) -> String {
    let mut sorted = fixes.to_vec();
    sorted.sort_by_key(|f| f.start);

    let mut result = String::with_capacity(source.len());
    let mut last = 0usize;
    for fix in &sorted {
        result.push_str(&source[last..fix.start]);
        result.push_str(&fix.replacement);
        last = fix.end;
    }
    result.push_str(&source[last..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_fixes(source: &str) -> Vec<Fix> {
        let line_index = LineIndex::new(source);
        find_unused_import_fixes(source, &line_index).expect("test source should parse")
    }

    fn none_fixes(source: &str) -> Vec<Fix> {
        let line_index = LineIndex::new(source);
        find_none_comparison_fixes(source, &line_index).expect("test source should parse")
    }

    // -- F401 -----------------------------------------------------------

    #[test]
    fn removes_unused_plain_import() {
        let source = "import os\nprint(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "print(1)\n");
    }

    #[test]
    fn removes_unused_from_import() {
        let source = "from collections import OrderedDict\nprint(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "print(1)\n");
    }

    #[test]
    fn leaves_used_import_alone() {
        let source = "import os\nprint(os.getcwd())\n";
        let f = import_fixes(source);
        assert!(f.is_empty());
        assert_eq!(apply_fixes(source, &f), source);
    }

    #[test]
    fn leaves_partially_used_from_import_alone() {
        // `a` is dead but `b` is used - this is a multi-name statement, so
        // the narrow prototype must not touch it at all.
        let source = "from x import a, b\nprint(b)\n";
        let f = import_fixes(source);
        assert!(f.is_empty());
    }

    #[test]
    fn leaves_underscore_bound_import_alone() {
        let source = "import os as _\nprint(1)\n";
        let f = import_fixes(source);
        assert!(f.is_empty());
    }

    #[test]
    fn leaves_multiline_import_alone() {
        let source = "from x import (\n    a,\n    b,\n)\nprint(1)\n";
        let f = import_fixes(source);
        assert!(f.is_empty(), "multi-line imports must be skipped, got {f:?}");
    }

    #[test]
    fn removes_only_the_dead_statement_among_several() {
        let source = "import os\nimport sys\nprint(sys.path)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "import sys\nprint(sys.path)\n");
    }

    #[test]
    fn no_fixes_on_clean_source() {
        let source = "import os\nprint(os.getcwd())\n";
        assert!(import_fixes(source).is_empty());
    }

    #[test]
    fn removes_indented_import_preserving_surrounding_lines() {
        // The import lives inside an `if` block; deleting its whole physical
        // line must not disturb the indentation of the lines around it.
        let source = "if True:\n    import os\n    print(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "if True:\n    print(1)\n");
    }

    #[test]
    fn removes_multiple_fully_dead_imports_in_one_pass() {
        let source = "import os\nimport json\nprint(1)\n";
        let f = import_fixes(source);
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
        let f = import_fixes(source);
        assert_eq!(
            f.len(),
            1,
            "documents that __all__ string references are not recognized as usage yet"
        );
    }

    // -- E711 -------------------------------------------------------------

    #[test]
    fn rewrites_eq_none_rhs() {
        let source = "x = 1\nif x == None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "x = 1\nif x is None:\n    pass\n");
    }

    #[test]
    fn rewrites_not_eq_none_rhs() {
        let source = "x = 1\nif x != None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(
            apply_fixes(source, &f),
            "x = 1\nif x is not None:\n    pass\n"
        );
    }

    #[test]
    fn rewrites_none_on_left_side() {
        let source = "x = 1\nif None == x:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "x = 1\nif None is x:\n    pass\n");
    }

    #[test]
    fn rewrites_against_a_call_expression() {
        let source = "if get_value() == None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_fixes(source, &f), "if get_value() is None:\n    pass\n");
    }

    #[test]
    fn leaves_is_none_alone() {
        let source = "x = 1\nif x is None:\n    pass\n";
        assert!(none_fixes(source).is_empty());
    }

    #[test]
    fn leaves_unrelated_comparison_alone() {
        let source = "x = 1\nif x == 5:\n    pass\n";
        assert!(none_fixes(source).is_empty());
    }

    #[test]
    fn leaves_chained_comparison_alone() {
        // Which `==` would we rewrite? Ambiguous enough to just refuse.
        let source = "x = 1\ny = 1\nif x == None == y:\n    pass\n";
        let f = none_fixes(source);
        assert!(f.is_empty(), "chained comparisons must be refused, got {f:?}");
    }

    #[test]
    fn leaves_no_space_around_operator_alone() {
        // Rewriting `x==None` to `xisNone` would corrupt the source.
        let source = "x = 1\nif x==None:\n    pass\n";
        let f = none_fixes(source);
        assert!(
            f.is_empty(),
            "operator with no surrounding whitespace must be refused, got {f:?}"
        );
    }

    #[test]
    fn rewrites_only_the_operator_not_the_whole_line() {
        let source = "if very_long_descriptive_name == None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(
            apply_fixes(source, &f),
            "if very_long_descriptive_name is None:\n    pass\n"
        );
    }

    // -- Composition: two independent fixers, one shared apply/diff engine --

    #[test]
    fn combined_fixes_from_two_rules_compose_correctly() {
        let source = "import os\nx = 1\nif x == None:\n    pass\n";
        let line_index = LineIndex::new(source);
        let mut all = find_unused_import_fixes(source, &line_index).unwrap();
        all.extend(find_none_comparison_fixes(source, &line_index).unwrap());
        assert_eq!(all.len(), 2);
        assert_eq!(apply_fixes(source, &all), "x = 1\nif x is None:\n    pass\n");
    }
}
