use std::collections::HashSet;

use rustpython_parser::ast::{self as ast, Ranged, StmtImport, StmtImportFrom, Suite, Visitor};
use rustpython_parser::Parse;

use crate::line_index::LineIndex;

/// A single safe text edit proposed by one of the fixers below.
///
/// A `Fix` is either a whole-line deletion (`replacement` empty, `start`/`end`
/// span a full physical line including its trailing newline) or an in-place
/// substitution or insertion (`start`/`end` span just the token being
/// replaced, or are equal for a pure insertion). All shapes are applied and
/// diffed through the same two functions - that reuse is the point: adding a
/// new fixer means writing a new `find_*` function, not a new apply/diff
/// engine.
///
/// `expected` is a snapshot of exactly what `start..end` contained in the
/// source *at the moment this fix was computed*. `apply_fixes` re-checks it
/// against whatever source it's actually given before touching anything -
/// see the note there for why.
#[derive(Debug, Clone)]
pub struct Fix {
    pub line: usize,
    pub description: String,
    start: usize,
    end: usize,
    replacement: String,
    expected: String,
}

impl Fix {
    fn new(source: &str, line: usize, description: String, start: usize, end: usize, replacement: String) -> Self {
        Self {
            line,
            description,
            start,
            end,
            replacement,
            expected: source[start..end].to_string(),
        }
    }
}

/// Why `apply_fixes` declined to touch a file, in enough detail that a
/// human or another agent can tell exactly what happened without guessing.
/// Deliberately not "clever" about any of these: no fuzzy matching, no
/// automatic reordering, no partial application. If it's ambiguous, refuse
/// and say why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The fix's target range no longer exists in the given source at all
    /// (e.g. the file got shorter, or the offset no longer lands on a
    /// character boundary).
    OutOfBounds { line: usize, description: String },
    /// The fix's target range still exists, but its contents differ from
    /// what was recorded when the fix was computed - the file changed
    /// since detection. This tool never attempts a "close enough" fuzzy
    /// re-apply; an exact match is required or the fix is refused.
    StaleContent {
        line: usize,
        description: String,
        expected: String,
        found: String,
    },
    /// Two fixes target overlapping byte ranges, or share the exact same
    /// start offset (e.g. two independent insertions at the same point).
    /// Which one should logically come first is ambiguous - and guessing
    /// wrong here isn't a cosmetic error, it can panic or scramble output
    /// - so neither is applied.
    Overlap {
        first_line: usize,
        first_description: String,
        second_line: usize,
        second_description: String,
    },
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::OutOfBounds { line, description } => write!(
                f,
                "line {line} ({description}): target range no longer exists in this file"
            ),
            RefusalReason::StaleContent {
                line,
                description,
                expected,
                found,
            } => write!(
                f,
                "line {line} ({description}): expected to find {expected:?} but the file now has {found:?} - it changed since this fix was computed"
            ),
            RefusalReason::Overlap {
                first_line,
                first_description,
                second_line,
                second_description,
            } => write!(
                f,
                "line {first_line} ({first_description}) collides with line {second_line} ({second_description}) - which should apply first is ambiguous"
            ),
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

        let del_start = line_start_offset(source, *start);
        let del_end = line_end_offset_inclusive_newline(source, *end);
        fixes.push(Fix::new(
            source,
            start_line,
            format!("remove unused import: {}", names.join(", ")),
            del_start,
            del_end,
            String::new(),
        ));
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
        fixes.push(Fix::new(
            source,
            line,
            format!("replace `{needle}` with `{replacement}` for None comparison"),
            op_start,
            op_end,
            replacement.to_string(),
        ));
    }

    fixes.sort_by_key(|f| f.start);
    Ok(fixes)
}

// ---------------------------------------------------------------------
// B006 - hoist a mutable default argument into a `None` guard
// ---------------------------------------------------------------------
//
// `def f(x=[]):` becomes:
//     def f(x=None):
//         if x is None:
//             x = []
//
// This is a fundamentally different shape of edit from the two fixers
// above: it *inserts* a new statement rather than deleting or substituting
// existing text. Each fixed function produces two `Fix`es - a same-line
// substitution (the default -> `None`) and a zero-width insertion (the new
// guard statement) - so `apply_fixes` needed no changes at all (a zero-width
// span is just an edit that removes nothing). `render_diff` is a different
// story: see the note above its insertion branch.

fn is_docstring_stmt(stmt: &ast::Stmt) -> bool {
    matches!(
        stmt,
        ast::Stmt::Expr(e) if matches!(e.value.as_ref(), ast::Expr::Constant(c) if matches!(c.value, ast::Constant::Str(_)))
    )
}

/// Returns the exact leading whitespace of the line `stmt_start` sits on,
/// or `None` if anything other than whitespace precedes it on that line
/// (e.g. `def f(x=[]): return x` on one line, or a `;`-separated compound
/// statement) - inserting a new sibling statement "before" code like that
/// can't be done safely as a text edit, so callers must refuse instead.
fn statement_indent(source: &str, stmt_start: usize) -> Option<&str> {
    let line_start = line_start_offset(source, stmt_start);
    let prefix = &source[line_start..stmt_start];
    prefix
        .chars()
        .all(|c| c == ' ' || c == '\t')
        .then_some(prefix)
}

struct MutableDefaultCollector<'a> {
    source: &'a str,
    line_index: &'a LineIndex,
    fixes: Vec<Fix>,
}

impl<'a> MutableDefaultCollector<'a> {
    fn new(source: &'a str, line_index: &'a LineIndex) -> Self {
        Self {
            source,
            line_index,
            fixes: Vec::new(),
        }
    }

    fn handle_function(&mut self, args: &ast::Arguments, body: &[ast::Stmt]) {
        let all_args = args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.kwonlyargs.iter());

        let mut mutable_defaults = Vec::new();
        for arg in all_args {
            let Some(default) = &arg.default else {
                continue;
            };
            let is_mutable_literal = matches!(
                default.as_ref(),
                ast::Expr::List(_) | ast::Expr::Dict(_) | ast::Expr::Set(_)
            );
            if is_mutable_literal {
                mutable_defaults.push((arg.def.arg.as_str(), default.as_ref()));
            }
        }

        // Refuse functions with more than one mutable default for this
        // first cut - one function needing two coordinated insertions adds
        // real risk (ordering, combined block layout) for little payoff
        // this early. Start small.
        if mutable_defaults.len() != 1 {
            return;
        }
        let (param_name, default_expr) = mutable_defaults[0];

        let default_start = default_expr.range().start().to_usize();
        let default_end = default_expr.range().end().to_usize();
        let (default_start_line, _) = self.line_index.line_col(default_start);
        let (default_end_line, _) = self.line_index.line_col(default_end);
        if default_start_line != default_end_line {
            // Multi-line literal default (rare) - not safe for this prototype.
            return;
        }
        let default_text = &self.source[default_start..default_end];

        // Insert the guard right after a leading docstring if there is one,
        // otherwise at the very start of the body.
        let has_docstring = body.first().is_some_and(is_docstring_stmt);
        let insert_before_index = if has_docstring { 1 } else { 0 };

        let (insert_at, indent) = if insert_before_index < body.len() {
            let anchor_start = body[insert_before_index].range().start().to_usize();
            let Some(indent) = statement_indent(self.source, anchor_start) else {
                return; // shares a line with other code - refuse.
            };
            (line_start_offset(self.source, anchor_start), indent)
        } else {
            // Body is a docstring and nothing else - append right after it.
            let last = &body[body.len() - 1];
            let anchor_start = last.range().start().to_usize();
            let Some(indent) = statement_indent(self.source, anchor_start) else {
                return;
            };
            let anchor_end = last.range().end().to_usize();
            (line_end_offset_inclusive_newline(self.source, anchor_end), indent)
        };

        let insert_text =
            format!("{indent}if {param_name} is None:\n{indent}{indent}{param_name} = {default_text}\n");
        let (insert_line, _) = self.line_index.line_col(insert_at);

        self.fixes.push(Fix::new(
            self.source,
            default_start_line,
            format!("replace mutable default for `{param_name}` with `None`"),
            default_start,
            default_end,
            "None".to_string(),
        ));
        self.fixes.push(Fix::new(
            self.source,
            insert_line,
            format!("initialize `{param_name}` inside the function body instead"),
            insert_at,
            insert_at,
            insert_text,
        ));
    }
}

impl<'a> Visitor for MutableDefaultCollector<'a> {
    fn visit_stmt_function_def(&mut self, node: ast::StmtFunctionDef) {
        self.handle_function(&node.args, &node.body);
        self.generic_visit_stmt_function_def(node);
    }

    fn visit_stmt_async_function_def(&mut self, node: ast::StmtAsyncFunctionDef) {
        self.handle_function(&node.args, &node.body);
        self.generic_visit_stmt_async_function_def(node);
    }
}

/// Finds mutable (`list`/`dict`/`set`) default arguments safe to hoist into
/// a `None`-guarded assignment inside the function body.
///
/// Scope is deliberately narrow: a function with more than one mutable
/// default is refused outright (start small); the default literal must sit
/// on a single physical line; and the statement the guard would be inserted
/// before must be the sole content of its own line (a one-line function
/// body like `def f(x=[]): return x` is refused rather than risk splicing
/// into shared code).
pub fn find_mutable_default_fixes(source: &str, line_index: &LineIndex) -> anyhow::Result<Vec<Fix>> {
    let suite: Suite = Suite::parse(source, "<fix>").map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut collector = MutableDefaultCollector::new(source, line_index);
    for stmt in suite {
        collector.visit_stmt(stmt);
    }

    let mut fixes = collector.fixes;
    fixes.sort_by_key(|f| f.start);
    Ok(fixes)
}

// ---------------------------------------------------------------------
// Shared diff/apply engine - identical for every fixer above.
// ---------------------------------------------------------------------

/// Renders a minimal diff of what each fix will change.
///
/// CRACK IN THE ABSTRACTION: this started as two cases - delete a whole
/// line, or substitute a span within one line - both of which fit neatly
/// because every edit lived on exactly one physical line. B006's guard
/// insertion breaks that assumption: it's a zero-width edit whose
/// replacement text is itself several *new* lines, with no corresponding
/// "before" line to diff against. The line-substitution branch below would
/// otherwise cram a multi-line block into one line of output. This needed
/// a genuinely new branch, not a reuse of the existing ones.
pub fn render_diff(source: &str, fixes: &[Fix]) -> String {
    let mut sorted = fixes.to_vec();
    sorted.sort_by_key(|f| f.start);

    let mut out = String::new();
    for fix in &sorted {
        if fix.start == fix.end && !fix.replacement.is_empty() {
            for inserted_line in fix.replacement.lines() {
                out.push_str(&format!("+ {:>4} | {}\n", fix.line, inserted_line));
            }
            out.push_str(&format!("       ^ {}\n", fix.description));
            continue;
        }

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

/// Applies the fixes to `source`, returning the new file contents - or every
/// reason it refused to, if any.
///
/// This is the actual safety boundary, not `--fix`'s diff preview: a
/// `Fix`'s `start`/`end` are just numbers by the time they get here, with no
/// guarantee the source handed to this function is the same one they were
/// computed against (a concurrent edit, a stale cache, a caller reusing an
/// old fix list - the reason doesn't matter). Before changing anything, this
/// verifies, for every fix:
///
/// - its range still exists in `source` at all (`OutOfBounds`),
/// - the text there still matches exactly what was recorded when the fix
///   was computed - no fuzzy "close enough" matching (`StaleContent`),
/// - it doesn't overlap, or start at the exact same point as, any other
///   fix in the batch (`Overlap`) - this includes two zero-width insertions
///   claiming the same spot, which is a real case: an unused import as a
///   function's first statement, in a function that also needs its mutable
///   default fixed, produces exactly this collision (F401 wants to delete
///   that line; B006 wants to insert new lines "before" that same
///   statement). Picking an order automatically risks doing it wrong
///   silently - or worse, panicking mid-write - so both are refused instead.
///
/// If *any* fix in the batch fails a check, none of them are applied - the
/// caller gets every reason back so a human (or another agent) can see
/// exactly why, rather than a partially-applied file or a bare crash.
pub fn apply_fixes(source: &str, fixes: &[Fix]) -> Result<String, Vec<RefusalReason>> {
    let mut sorted = fixes.to_vec();
    sorted.sort_by_key(|f| f.start);

    let mut reasons = Vec::new();

    for pair in sorted.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.end > b.start || a.start == b.start {
            reasons.push(RefusalReason::Overlap {
                first_line: a.line,
                first_description: a.description.clone(),
                second_line: b.line,
                second_description: b.description.clone(),
            });
        }
    }

    for fix in &sorted {
        match source.get(fix.start..fix.end) {
            None => reasons.push(RefusalReason::OutOfBounds {
                line: fix.line,
                description: fix.description.clone(),
            }),
            Some(found) if found != fix.expected => reasons.push(RefusalReason::StaleContent {
                line: fix.line,
                description: fix.description.clone(),
                expected: fix.expected.clone(),
                found: found.to_string(),
            }),
            Some(_) => {}
        }
    }

    if !reasons.is_empty() {
        return Err(reasons);
    }

    let mut result = String::with_capacity(source.len());
    let mut last = 0usize;
    for fix in &sorted {
        result.push_str(&source[last..fix.start]);
        result.push_str(&fix.replacement);
        last = fix.end;
    }
    result.push_str(&source[last..]);
    Ok(result)
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

    /// Applies fixes and unwraps, for tests where the fixes are computed
    /// fresh against the exact same source they're applied to - staleness
    /// and overlap simply can't occur by construction, so a failure here
    /// would mean a real bug, not an expected refusal.
    fn apply_clean(source: &str, fixes: &[Fix]) -> String {
        apply_fixes(source, fixes).expect("fixes computed fresh against this source must apply cleanly")
    }

    // -- F401 -----------------------------------------------------------

    #[test]
    fn removes_unused_plain_import() {
        let source = "import os\nprint(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_clean(source, &f), "print(1)\n");
    }

    #[test]
    fn removes_unused_from_import() {
        let source = "from collections import OrderedDict\nprint(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_clean(source, &f), "print(1)\n");
    }

    #[test]
    fn leaves_used_import_alone() {
        let source = "import os\nprint(os.getcwd())\n";
        let f = import_fixes(source);
        assert!(f.is_empty());
        assert_eq!(apply_clean(source, &f), source);
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
        assert_eq!(apply_clean(source, &f), "import sys\nprint(sys.path)\n");
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
        assert_eq!(apply_clean(source, &f), "if True:\n    print(1)\n");
    }

    #[test]
    fn removes_multiple_fully_dead_imports_in_one_pass() {
        let source = "import os\nimport json\nprint(1)\n";
        let f = import_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(apply_clean(source, &f), "print(1)\n");
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
        assert_eq!(apply_clean(source, &f), "x = 1\nif x is None:\n    pass\n");
    }

    #[test]
    fn rewrites_not_eq_none_rhs() {
        let source = "x = 1\nif x != None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(
            apply_clean(source, &f),
            "x = 1\nif x is not None:\n    pass\n"
        );
    }

    #[test]
    fn rewrites_none_on_left_side() {
        let source = "x = 1\nif None == x:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_clean(source, &f), "x = 1\nif None is x:\n    pass\n");
    }

    #[test]
    fn rewrites_against_a_call_expression() {
        let source = "if get_value() == None:\n    pass\n";
        let f = none_fixes(source);
        assert_eq!(f.len(), 1);
        assert_eq!(apply_clean(source, &f), "if get_value() is None:\n    pass\n");
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
            apply_clean(source, &f),
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
        assert_eq!(apply_clean(source, &all), "x = 1\nif x is None:\n    pass\n");
    }

    // -- B006 ---------------------------------------------------------------

    fn mutable_default_fixes(source: &str) -> Vec<Fix> {
        let line_index = LineIndex::new(source);
        find_mutable_default_fixes(source, &line_index).expect("test source should parse")
    }

    #[test]
    fn rewrites_list_default_after_docstring() {
        let source = "def add(item, bucket=[]):\n    \"\"\"Doc.\"\"\"\n    bucket.append(item)\n    return bucket\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def add(item, bucket=None):\n    \"\"\"Doc.\"\"\"\n    if bucket is None:\n        bucket = []\n    bucket.append(item)\n    return bucket\n"
        );
    }

    #[test]
    fn rewrites_dict_default_with_no_docstring() {
        let source = "def f(x={}):\n    return x\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def f(x=None):\n    if x is None:\n        x = {}\n    return x\n"
        );
    }

    #[test]
    fn rewrites_set_default() {
        let source = "def f(x={1, 2}):\n    return x\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def f(x=None):\n    if x is None:\n        x = {1, 2}\n    return x\n"
        );
    }

    #[test]
    fn rewrites_default_in_async_function() {
        let source = "async def f(x=[]):\n    return x\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "async def f(x=None):\n    if x is None:\n        x = []\n    return x\n"
        );
    }

    #[test]
    fn rewrites_keyword_only_default() {
        let source = "def f(*, x=[]):\n    return x\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def f(*, x=None):\n    if x is None:\n        x = []\n    return x\n"
        );
    }

    #[test]
    fn rewrites_when_body_is_docstring_only() {
        // No statement to insert "before" - the guard must be appended
        // right after the docstring's own line instead.
        let source = "def f(x=[]):\n    \"\"\"Doc.\"\"\"\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def f(x=None):\n    \"\"\"Doc.\"\"\"\n    if x is None:\n        x = []\n"
        );
    }

    #[test]
    fn preserves_tab_indentation() {
        let source = "def f(x=[]):\n\treturn x\n";
        let f = mutable_default_fixes(source);
        assert_eq!(f.len(), 2);
        assert_eq!(
            apply_clean(source, &f),
            "def f(x=None):\n\tif x is None:\n\t\tx = []\n\treturn x\n"
        );
    }

    #[test]
    fn ignores_none_default() {
        let source = "def f(x=None):\n    return x\n";
        assert!(mutable_default_fixes(source).is_empty());
    }

    #[test]
    fn ignores_scalar_default() {
        let source = "def f(x=5):\n    return x\n";
        assert!(mutable_default_fixes(source).is_empty());
    }

    #[test]
    fn refuses_function_with_two_mutable_defaults() {
        // Start small: two coordinated insertions in one function is
        // deliberately out of scope for this first cut.
        let source = "def f(x=[], y={}):\n    return x, y\n";
        let f = mutable_default_fixes(source);
        assert!(
            f.is_empty(),
            "functions with >1 mutable default must be refused, got {f:?}"
        );
    }

    #[test]
    fn refuses_multiline_literal_default() {
        let source = "def f(x=[\n    1,\n]):\n    return x\n";
        let f = mutable_default_fixes(source);
        assert!(
            f.is_empty(),
            "multi-line literal defaults must be refused, got {f:?}"
        );
    }

    #[test]
    fn refuses_single_line_function_body() {
        // The `def` line and the body share one physical line - there is no
        // safe place to insert a new sibling statement as a text edit.
        let source = "def f(x=[]): return x\n";
        let f = mutable_default_fixes(source);
        assert!(
            f.is_empty(),
            "one-line function bodies must be refused, got {f:?}"
        );
    }

    // -- Composition: all three fixers together ------------------------------

    #[test]
    fn combined_fixes_from_three_rules_compose_correctly() {
        let source = "import os\n\n\ndef get(x, cache={}):\n    if x == None:\n        return None\n    return cache.get(x)\n";
        let line_index = LineIndex::new(source);
        let mut all = find_unused_import_fixes(source, &line_index).unwrap();
        all.extend(find_none_comparison_fixes(source, &line_index).unwrap());
        all.extend(find_mutable_default_fixes(source, &line_index).unwrap());
        assert_eq!(all.len(), 4); // 1 import delete + 1 compare rewrite + 2 for the mutable default

        let fixed = apply_clean(source, &all);
        assert_eq!(
            fixed,
            "\n\ndef get(x, cache=None):\n    if cache is None:\n        cache = {}\n    if x is None:\n        return None\n    return cache.get(x)\n"
        );
    }

    // -- Hardened apply engine: explainable refusal ------------------------

    #[test]
    fn refuses_two_fixes_that_overlap() {
        let source = "x = 1\n";
        let fix_a = Fix::new(source, 1, "a".to_string(), 0, 5, "y".to_string());
        let fix_b = Fix::new(source, 1, "b".to_string(), 3, 5, "z".to_string());
        let err = apply_fixes(source, &[fix_a, fix_b]).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(matches!(err[0], RefusalReason::Overlap { .. }));
        // The reason must actually say something a human can act on, not
        // just "no" - both fixes' own descriptions should appear in it.
        let message = err[0].to_string();
        assert!(message.contains('a') && message.contains('b'));
    }

    #[test]
    fn refuses_two_insertions_at_the_identical_point() {
        // Two zero-width edits claiming the exact same offset - overlap by
        // the `end > start` check alone wouldn't catch this (both spans are
        // empty), so this is exactly the "shared start" branch.
        let source = "x = 1\n";
        let fix_a = Fix::new(source, 1, "insert A".to_string(), 3, 3, "AAA".to_string());
        let fix_b = Fix::new(source, 1, "insert B".to_string(), 3, 3, "BBB".to_string());
        let err = apply_fixes(source, &[fix_a, fix_b]).unwrap_err();
        assert!(matches!(err[0], RefusalReason::Overlap { .. }));
    }

    #[test]
    fn refuses_and_explains_a_real_cross_fixer_collision() {
        // Not synthetic: an unused import as a function's very first
        // statement, in a function that also needs its mutable default
        // fixed. F401 wants to delete that line; B006 wants to insert its
        // guard "before" that same statement - both land at the same
        // offset. Before this hardening, this exact input panicked the
        // whole tool (`byte range starts at 27 but ends at 13`) instead of
        // refusing. Confirmed by reproducing it against the CLI before
        // writing this test.
        let source = "def f(x=[]):\n    import os\n    return x\n";
        let line_index = LineIndex::new(source);
        let mut fixes = find_unused_import_fixes(source, &line_index).unwrap();
        fixes.extend(find_mutable_default_fixes(source, &line_index).unwrap());
        assert_eq!(fixes.len(), 3);

        let err = apply_fixes(source, &fixes).unwrap_err();
        assert!(
            err.iter().any(|r| matches!(r, RefusalReason::Overlap { .. })),
            "expected an Overlap refusal, got {err:?}"
        );
    }

    #[test]
    fn refuses_stale_content_instead_of_corrupting_it() {
        // Compute a fix against the original source, then "apply" it to a
        // version of the file that changed in the meantime - simulating a
        // concurrent edit or a caller reusing an old fix list.
        let original = "import os\nprint(1)\n";
        let f = import_fixes(original);
        assert_eq!(f.len(), 1);

        let changed_on_disk = "import sys\nprint(1)\n"; // same shape, different import
        let err = apply_fixes(changed_on_disk, &f).unwrap_err();
        assert_eq!(err.len(), 1);
        match &err[0] {
            RefusalReason::StaleContent { expected, found, .. } => {
                assert_eq!(expected, "import os\n");
                // "sys" is one byte longer than "os", so the same [start,end)
                // byte range now lands one character short of the newline -
                // itself a small illustration of why byte-range reuse across
                // different content is unsafe without an exact-match check.
                assert_eq!(found, "import sys");
            }
            other => panic!("expected StaleContent, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_close_but_not_exact_match_rather_than_fuzzy_applying() {
        // The kind of "diff that partially matches" a naive patch tool
        // might be tempted to fuzzy-apply anyway: only a single character
        // differs (an extra space), so a "close enough" heuristic could
        // easily justify sliding the edit onto it. This tool does not do
        // that - an exact match is required, or it refuses.
        let original = "x = 1\nif x == None:\n    pass\n";
        let f = none_fixes(original);
        assert_eq!(f.len(), 1);

        let slightly_different = "x = 1\nif x  == None:\n    pass\n"; // extra space before `==`
        let err = apply_fixes(slightly_different, &f).unwrap_err();
        assert!(
            err.iter().any(|r| matches!(r, RefusalReason::StaleContent { .. })
                || matches!(r, RefusalReason::OutOfBounds { .. })),
            "a near-miss must be refused, not silently patched: {err:?}"
        );
    }

    #[test]
    fn refuses_out_of_bounds_rather_than_panicking() {
        // The file got shorter since the fix was computed - the recorded
        // range doesn't exist in the new source at all.
        let original = "import os\nprint(1)\n";
        let f = import_fixes(original);
        assert_eq!(f.len(), 1);

        let truncated = "im";
        let err = apply_fixes(truncated, &f).unwrap_err();
        assert!(matches!(err[0], RefusalReason::OutOfBounds { .. }));
    }

    #[test]
    fn clean_multi_fixer_batches_still_apply_with_no_refusals() {
        // Sanity check that hardening the engine didn't make it paranoid
        // about the ordinary, non-colliding case that composition relies on.
        let source = "import os\nx = 1\nif x == None:\n    pass\n";
        let line_index = LineIndex::new(source);
        let mut all = find_unused_import_fixes(source, &line_index).unwrap();
        all.extend(find_none_comparison_fixes(source, &line_index).unwrap());
        assert!(apply_fixes(source, &all).is_ok());
    }
}
