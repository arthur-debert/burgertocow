//! End-to-end fixture tests.
//!
//! Each test reads a template + JSON data + a "modified" version of the
//! rendered output from `tests/fixtures/`, runs the reverse-diff
//! pipeline, and asserts on the resulting diff. These complement the
//! per-module unit tests by exercising the full public surface against
//! realistic files.

use burgertocow::{generate_diff, generate_diff_with_markers, ConflictMarkers, Tracker};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

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

// ---------------------------------------------------------------------------
// Custom global functions (embedder-registered; models dodot's `secret()`).
// ---------------------------------------------------------------------------
//
// These tests exercise the tracker + diff pipeline when the minijinja
// environment has a caller-registered global function whose return value
// stands in for a sensitive, externally-resolved datum (a vault secret, a
// looked-up credential, etc.). The burgertocow contract we want to verify:
//
//   1. Registering a global function on `env_mut()` does not interfere with
//      the tracking formatter — every call site still emits one balanced
//      marker pair.
//   2. When only the *resolved value* differs between render and modified,
//      the diff is empty (the edit is classified as pure-data).
//   3. Static edits adjacent to a function call still map back to the
//      template correctly.
//   4. Values containing newlines (multi-line keys, PEM blocks, …) do not
//      desynchronise marker accounting.

/// Build a tracker with a `secret(uri)` global that looks up `uri` in
/// `values`. The `Arc<Mutex<…>>` indirection lets a test mutate the map
/// between two renders to simulate a vault rotation.
fn make_secret_tracker(values: Arc<Mutex<HashMap<String, String>>>) -> Tracker {
    let mut tracker = Tracker::new();
    let store = values.clone();
    tracker.env_mut().add_function("secret", move |uri: &str| {
        store
            .lock()
            .unwrap()
            .get(uri)
            .cloned()
            .unwrap_or_else(|| format!("<unresolved: {uri}>"))
    });
    tracker
}

#[test]
fn secret_function_emits_balanced_markers() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://Personal/GitHub/token".to_string(),
        "ghp_ABC123".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    tracker
        .add_template(
            "t",
            r#"GH_TOKEN="{{ secret('op://Personal/GitHub/token') }}"
"#,
        )
        .unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();
    assert_eq!(tracked.output(), "GH_TOKEN=\"ghp_ABC123\"\n");
    assert_eq!(tracked.tracked().matches('\x1e').count(), 1);
    assert_eq!(tracked.tracked().matches('\x1f').count(), 1);
}

#[test]
fn secret_value_rotation_produces_empty_diff() {
    // Render once with the old value, then (separately) construct what the
    // rendered file would look like after the vault rotated. The reverse
    // diff must classify this as a pure-data change and return "".
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://Personal/GitHub/token".to_string(),
        "ghp_OLD".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values.clone());
    let src = r#"GH_TOKEN="{{ secret('op://Personal/GitHub/token') }}"
"#;
    tracker.add_template("t", src).unwrap();
    let tracked_old = tracker.render("t", serde_json::json!({})).unwrap();
    assert_eq!(tracked_old.output(), "GH_TOKEN=\"ghp_OLD\"\n");

    // Simulate the deployed file now containing a rotated value — this is
    // exactly what a downstream tool would see after the vault changed.
    let modified = "GH_TOKEN=\"ghp_NEW_ROTATED_VALUE\"\n";
    let diff = generate_diff(src, &tracked_old, modified);
    assert_eq!(
        diff, "",
        "secret-only change must be classified as pure data; got: {diff}"
    );
}

#[test]
fn static_edit_next_to_secret_maps_to_template() {
    // A two-line config: one plaintext line, one secret-bearing line. The
    // user edits only the plaintext line. The reverse diff must attribute
    // the edit to the template and must NOT leak the resolved secret value
    // into the emitted diff.
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://DB/password".to_string(),
        "s3cret-v4lue".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = r#"host = "localhost"
password = "{{ secret('op://DB/password') }}"
"#;
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();
    assert_eq!(
        tracked.output(),
        "host = \"localhost\"\npassword = \"s3cret-v4lue\"\n"
    );

    // User edits the host on the deployed file; the secret line is left
    // untouched at its resolved value.
    let modified = "host = \"production.db.internal\"\npassword = \"s3cret-v4lue\"\n";
    let diff = generate_diff(src, &tracked, modified);
    assert!(
        diff.contains("-host = \"localhost\""),
        "expected removal of original host line; got: {diff}"
    );
    assert!(
        diff.contains("+host = \"production.db.internal\""),
        "expected addition of new host line; got: {diff}"
    );
    // The resolved secret value must never appear in the template-space
    // diff — the diff targets the template, which has {{ secret(...) }}.
    assert!(
        !diff.contains("s3cret-v4lue"),
        "resolved secret leaked into diff: {diff}"
    );
}

#[test]
fn multi_line_secret_value_keeps_markers_balanced() {
    // A secret whose value contains newlines (PEM block, multi-line SSH
    // key, etc.). The tracking formatter wraps the *whole* value atomically
    // between one marker pair, so marker accounting must stay 1:1.
    let pem = "-----BEGIN PRIVATE KEY-----\nMIIBVgIBADANBgkqhki\nrandomlinetwo\n-----END PRIVATE KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    tracker
        .add_template("t", "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n")
        .unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    assert_eq!(
        tracked.output(),
        format!("KEY<<EOF\n{pem}\nEOF\n"),
        "multi-line secret did not render verbatim"
    );
    assert_eq!(
        tracked.tracked().matches('\x1e').count(),
        1,
        "expected exactly one VAR_START for a single secret() call"
    );
    assert_eq!(
        tracked.tracked().matches('\x1f').count(),
        1,
        "expected exactly one VAR_END for a single secret() call"
    );
}

#[test]
fn multi_line_secret_rotation_produces_empty_diff() {
    // Rotating a multi-line secret must still be classified as a pure
    // data change. This is the stress test for marker-span tracking over
    // newlines.
    let old_pem =
        "-----BEGIN KEY-----\nline1-old\nline2-old\n-----END KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        old_pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let new_pem =
        "-----BEGIN KEY-----\nline1-NEW\nline2-NEW-LONGER\nline3-NEW\n-----END KEY-----";
    let modified = format!("KEY<<EOF\n{new_pem}\nEOF\n");
    let diff = generate_diff(src, &tracked, &modified);
    assert_eq!(
        diff, "",
        "multi-line secret rotation should be pure-data (empty diff); got: {diff}"
    );
}

#[test]
fn static_edit_after_multi_line_secret_maps_to_template() {
    // Static edit on a line that appears *after* a multi-line secret. This
    // is the mapping invariant under test: newlines inside a variable span
    // must not shift the render→template alignment for subsequent lines.
    let pem = "-----BEGIN KEY-----\nline1\nline2\nline3\n-----END KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n# end of file\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    // User edits only the trailing comment line.
    let modified = format!("KEY<<EOF\n{pem}\nEOF\n# tail comment\n");
    let diff = generate_diff(src, &tracked, &modified);
    assert!(
        diff.contains("-# end of file"),
        "expected removal of original comment line; got: {diff}"
    );
    assert!(
        diff.contains("+# tail comment"),
        "expected addition of new comment line; got: {diff}"
    );
    // No part of the PEM body should appear in the template diff.
    assert!(
        !diff.contains("line1") && !diff.contains("line2") && !diff.contains("line3"),
        "secret body leaked into template diff: {diff}"
    );
}

// ---------------------------------------------------------------------------
// Custom conflict markers.
// ---------------------------------------------------------------------------

#[test]
fn custom_conflict_markers_replace_defaults() {
    // A loop whose iterations were edited inconsistently always produces a
    // conflict block. Verify that `generate_diff_with_markers` uses the
    // caller-supplied boundary strings and does NOT emit the default ones.
    let mut tracker = Tracker::new();
    tracker
        .add_template("t", "{% for i in items %}- {{ i }}\n{% endfor %}")
        .unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"items": ["Apple", "Banana"]}))
        .unwrap();

    let markers = ConflictMarkers::new(
        ">>>>>> dodot-conflict\n",
        "======\n",
        "<<<<<< dodot-conflict\n",
    );
    let diff = generate_diff_with_markers(
        "{% for i in items %}- {{ i }}\n{% endfor %}",
        &tracked,
        "* Apple\n! Banana\n",
        &markers,
    );

    assert!(
        diff.contains(">>>>>> dodot-conflict"),
        "custom start marker missing: {diff}"
    );
    assert!(diff.contains("======"), "custom mid marker missing: {diff}");
    assert!(
        diff.contains("<<<<<< dodot-conflict"),
        "custom end marker missing: {diff}"
    );
    assert!(
        !diff.contains("diff decision needed"),
        "default marker leaked into custom-marker output: {diff}"
    );
}

#[test]
fn custom_markers_do_not_affect_non_conflict_output() {
    // For a plain static-text edit (no conflict), the markers must be
    // irrelevant — the output is a unified diff identical to what
    // `generate_diff` would produce.
    let mut tracker = Tracker::new();
    tracker.add_template("t", "Hello {{ u }}!\nBye.").unwrap();
    let tracked = tracker.render("t", serde_json::json!({"u": "A"})).unwrap();

    let markers = ConflictMarkers::new("X", "Y", "Z");
    let with = generate_diff_with_markers(
        "Hello {{ u }}!\nBye.",
        &tracked,
        "Hello A!\nBye for now.",
        &markers,
    );
    let without = generate_diff("Hello {{ u }}!\nBye.", &tracked, "Hello A!\nBye for now.");
    assert_eq!(with, without);
    assert!(!with.contains('X') && !with.contains('Y') && !with.contains('Z'));
}

#[test]
fn default_markers_match_generate_diff() {
    // Passing `ConflictMarkers::default()` explicitly must produce
    // byte-identical output to the legacy `generate_diff` helper — this is
    // the backstop test for the refactor.
    let mut tracker = Tracker::new();
    tracker
        .add_template("t", "{% for i in items %}- {{ i }}\n{% endfor %}")
        .unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"items": ["Apple", "Banana"]}))
        .unwrap();
    let legacy = generate_diff(
        "{% for i in items %}- {{ i }}\n{% endfor %}",
        &tracked,
        "* Apple\n! Banana\n",
    );
    let modern = generate_diff_with_markers(
        "{% for i in items %}- {{ i }}\n{% endfor %}",
        &tracked,
        "* Apple\n! Banana\n",
        &ConflictMarkers::default(),
    );
    assert_eq!(legacy, modern);
}
