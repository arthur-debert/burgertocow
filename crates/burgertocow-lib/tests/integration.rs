//! End-to-end fixture tests.
//!
//! Each test reads a template + JSON data + a "modified" version of the
//! rendered output from `tests/fixtures/`, runs the reverse-diff
//! pipeline, and asserts on the resulting diff. These complement the
//! per-module unit tests by exercising the full public surface against
//! realistic files.

use burgertocow::{generate_diff, Tracker};
use std::fs;

fn run_diff(template_file: &str, data_file: &str, modified_file: &str) -> String {
    let t_str = fs::read_to_string(template_file).unwrap();
    let d_str = fs::read_to_string(data_file).unwrap();
    let m_str = fs::read_to_string(modified_file).unwrap();
    let ctx: serde_json::Value = serde_json::from_str(&d_str).unwrap();

    let mut tracker = Tracker::new();
    tracker.add_template("test", &t_str).unwrap();
    let tracked = tracker.render("test", &ctx).unwrap();

    generate_diff(&t_str, &tracked, &m_str)
}

#[test]
fn test_template_text_modification() {
    let diff = run_diff(
        "../../tests/fixtures/simple.md",
        "../../tests/fixtures/simple_data.json",
        "../../tests/fixtures/simple_mod_text.md",
    );
    assert!(diff.contains("-Welcome to the template."));
    assert!(diff.contains("+Welcome to the modified template."));
}

#[test]
fn test_pure_variable_modification() {
    let diff = run_diff(
        "../../tests/fixtures/simple.md",
        "../../tests/fixtures/simple_data.json",
        "../../tests/fixtures/simple_mod_var.md",
    );
    assert_eq!(diff.trim(), "");
}

#[test]
fn test_ambiguous_loop_modification() {
    let diff = run_diff(
        "../../tests/fixtures/loop.md",
        "../../tests/fixtures/loop_data.json",
        "../../tests/fixtures/loop_mod.md",
    );
    assert!(diff.contains("<<<< diff decision needed: start >>>>"));
    assert!(diff.contains("resolve this manually"));
}

/// When every loop iteration receives the *same* static-text edit, the
/// loop-iteration fallback should consolidate them into one template
/// change rather than flagging a conflict. This is the scenario the
/// heuristic was designed to rescue from the "diff decision needed"
/// outcome.
#[test]
fn test_consistent_loop_modification_is_consolidated() {
    let diff = run_diff(
        "../../tests/fixtures/loop_consistent.md",
        "../../tests/fixtures/loop_consistent_data.json",
        "../../tests/fixtures/loop_consistent_mod.md",
    );
    assert!(
        !diff.contains("diff decision needed"),
        "consistent loop edit should not require manual resolution: {diff}"
    );
    // The template line contains "- {{ i }}"; the modified version uses
    // "* ". The unified diff must show the prefix flipping.
    assert!(diff.contains("- {{ i }}"), "old prefix missing: {diff}");
    assert!(diff.contains("* {{ i }}"), "new prefix missing: {diff}");
}

#[test]
fn test_conditional_block_template_edit() {
    let diff = run_diff(
        "../../tests/fixtures/conditional.md",
        "../../tests/fixtures/conditional_data.json",
        "../../tests/fixtures/conditional_mod.md",
    );
    // Static text "Welcome." changed to "Greetings." — template edit.
    assert!(diff.contains("-Welcome."));
    assert!(diff.contains("+Greetings."));
}

#[test]
fn test_unicode_template_edit() {
    let diff = run_diff(
        "../../tests/fixtures/unicode.md",
        "../../tests/fixtures/unicode_data.json",
        "../../tests/fixtures/unicode_mod.md",
    );
    // "さようなら" line changed to "また来てね" — template-only edit.
    assert!(
        diff.contains("-さようなら"),
        "original line missing: {diff}"
    );
    assert!(diff.contains("+また来てね"), "new line missing: {diff}");
}

#[test]
fn tracker_output_equals_plain_minijinja_render() {
    let mut tracker = Tracker::new();
    tracker
        .add_template("t", "Hi {{ user }}, {{ items | length }} items")
        .unwrap();
    let ctx = serde_json::json!({ "user": "A", "items": [1, 2, 3] });
    let tracked = tracker.render("t", &ctx).unwrap();
    assert_eq!(tracked.output(), "Hi A, 3 items");
    assert!(tracked.tracked().contains('\x1e'));
    assert!(tracked.tracked().contains('\x1f'));
}
