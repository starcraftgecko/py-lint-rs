use std::path::Path;

use super::check;
use crate::diagnostics::Diagnostic;
use crate::line_index::LineIndex;

fn lint(source: &str) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(source);
    check(Path::new("<test>.py"), source, &line_index).expect("test source should parse")
}

fn has(diags: &[Diagnostic], code: &str) -> bool {
    diags.iter().any(|d| d.code == code)
}

fn count(diags: &[Diagnostic], code: &str) -> usize {
    diags.iter().filter(|d| d.code == code).count()
}

// ---------------------------------------------------------------------
// F401 - unused import
// ---------------------------------------------------------------------

#[test]
fn f401_flags_unused_plain_import() {
    let diags = lint("import os\n");
    assert!(has(&diags, "F401"));
}

#[test]
fn f401_flags_unused_from_import() {
    let diags = lint("from collections import OrderedDict\n");
    assert!(has(&diags, "F401"));
}

#[test]
fn f401_flags_unused_aliased_import() {
    let diags = lint("import sys as s\n");
    assert!(has(&diags, "F401"));
}

#[test]
fn f401_ignores_used_plain_import() {
    let diags = lint("import os\nprint(os.getcwd())\n");
    assert!(!has(&diags, "F401"));
}

#[test]
fn f401_ignores_used_from_import() {
    let diags = lint("from collections import OrderedDict\nd = OrderedDict()\n");
    assert!(!has(&diags, "F401"));
}

#[test]
fn f401_ignores_used_aliased_import() {
    let diags = lint("import sys as s\nprint(s.path)\n");
    assert!(!has(&diags, "F401"));
}

#[test]
fn f401_ignores_star_import() {
    let diags = lint("from os import *\n");
    assert!(!has(&diags, "F401"));
}

#[test]
fn f401_ignores_underscore_alias() {
    let diags = lint("import os as _\n");
    assert!(!has(&diags, "F401"));
}

// ---------------------------------------------------------------------
// B006 - mutable default argument
// ---------------------------------------------------------------------

#[test]
fn b006_flags_list_default() {
    let diags = lint("def f(x=[]):\n    pass\n");
    assert!(has(&diags, "B006"));
}

#[test]
fn b006_flags_dict_default() {
    let diags = lint("def f(x={}):\n    pass\n");
    assert!(has(&diags, "B006"));
}

#[test]
fn b006_flags_set_default() {
    let diags = lint("def f(x={1, 2}):\n    pass\n");
    assert!(has(&diags, "B006"));
}

#[test]
fn b006_flags_default_in_async_function() {
    let diags = lint("async def f(x=[]):\n    pass\n");
    assert!(has(&diags, "B006"));
}

#[test]
fn b006_flags_keyword_only_default() {
    let diags = lint("def f(*, x=[]):\n    pass\n");
    assert!(has(&diags, "B006"));
}

#[test]
fn b006_ignores_none_default() {
    let diags = lint("def f(x=None):\n    pass\n");
    assert!(!has(&diags, "B006"));
}

#[test]
fn b006_ignores_tuple_default() {
    let diags = lint("def f(x=()):\n    pass\n");
    assert!(!has(&diags, "B006"));
}

#[test]
fn b006_ignores_scalar_default() {
    let diags = lint("def f(x=5):\n    pass\n");
    assert!(!has(&diags, "B006"));
}

// ---------------------------------------------------------------------
// E711 - comparison to None with == / !=
// ---------------------------------------------------------------------

#[test]
fn e711_flags_eq_none_rhs() {
    let diags = lint("x = 1\nif x == None:\n    pass\n");
    assert!(has(&diags, "E711"));
}

#[test]
fn e711_flags_none_lhs() {
    let diags = lint("x = 1\nif None != x:\n    pass\n");
    assert!(has(&diags, "E711"));
}

#[test]
fn e711_ignores_is_none() {
    let diags = lint("x = 1\nif x is None:\n    pass\n");
    assert!(!has(&diags, "E711"));
}

#[test]
fn e711_ignores_is_not_none() {
    let diags = lint("x = 1\nif x is not None:\n    pass\n");
    assert!(!has(&diags, "E711"));
}

#[test]
fn e711_ignores_unrelated_comparison() {
    let diags = lint("x = 1\nif x == 5:\n    pass\n");
    assert!(!has(&diags, "E711"));
}

// ---------------------------------------------------------------------
// E722 - bare except
// ---------------------------------------------------------------------

#[test]
fn e722_flags_bare_except() {
    let diags = lint("try:\n    pass\nexcept:\n    pass\n");
    assert!(has(&diags, "E722"));
}

#[test]
fn e722_ignores_typed_except() {
    let diags = lint("try:\n    pass\nexcept ValueError:\n    pass\n");
    assert!(!has(&diags, "E722"));
}

#[test]
fn e722_ignores_typed_except_with_name() {
    let diags = lint("try:\n    pass\nexcept Exception as e:\n    print(e)\n");
    assert!(!has(&diags, "E722"));
}

// ---------------------------------------------------------------------
// E501 - line too long
// ---------------------------------------------------------------------

#[test]
fn e501_flags_line_over_100_chars() {
    let long_comment = format!("# {}\n", "a".repeat(105));
    let diags = lint(&long_comment);
    assert!(has(&diags, "E501"));
}

#[test]
fn e501_ignores_line_at_exactly_100_chars() {
    // "# " (2 chars) + 98 'a's = 100 chars total.
    let line = format!("# {}\n", "a".repeat(98));
    assert_eq!(line.lines().next().unwrap().chars().count(), 100);
    let diags = lint(&line);
    assert!(!has(&diags, "E501"));
}

#[test]
fn e501_ignores_short_line() {
    let diags = lint("x = 1\n");
    assert!(!has(&diags, "E501"));
}

// ---------------------------------------------------------------------
// Regression: locks in current behavior against the committed example
// fixtures so future rule changes can't silently alter known-good output.
// ---------------------------------------------------------------------

#[test]
fn examples_bad_py_matches_known_findings() {
    let source = include_str!("../../examples/bad.py");
    let diags = lint(source);

    assert_eq!(count(&diags, "F401"), 2);
    assert_eq!(count(&diags, "B006"), 1);
    assert_eq!(count(&diags, "E711"), 1);
    assert_eq!(count(&diags, "E722"), 1);
    assert_eq!(diags.len(), 5);
}

#[test]
fn examples_good_py_is_clean() {
    let source = include_str!("../../examples/good.py");
    let diags = lint(source);
    assert!(diags.is_empty(), "expected no issues, got {diags:?}");
}
