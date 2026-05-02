//! End-to-end fixture tests.
//!
//! Each test reads a template + JSON data + a "modified" version of the
//! rendered output from `tests/fixtures/`, runs the reverse-diff
//! pipeline, and asserts on the resulting diff. These complement the
//! per-module unit tests by exercising the full public surface against
//! realistic files.

// Mask tests pass `&[Range<usize>; 1]` literals like `[1..2]` to slice
// parameters intentionally — that's the contract shape, not a "I meant
// to expand the range into a Vec" mistake.
#![allow(clippy::single_range_in_vec_init)]

use burgertocow::{
    generate_diff, generate_diff_with_markers, generate_diff_with_markers_opts, ConflictMarkers,
    DiffOptions, Tracker, VAR_END, VAR_START,
};
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
    let old_pem = "-----BEGIN KEY-----\nline1-old\nline2-old\n-----END KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        old_pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let new_pem = "-----BEGIN KEY-----\nline1-NEW\nline2-NEW-LONGER\nline3-NEW\n-----END KEY-----";
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

// ---------------------------------------------------------------------------
// Mask deployed-file line ranges (issue #13).
//
// These tests pin down the contract of `generate_diff_with_markers_opts`:
// callers (initially dodot, eventually any tool with a per-file sidecar of
// "ignore these lines") supply line ranges that should be treated as if
// they always matched the cached render. Each test corresponds to one of
// the four use cases enumerated in the issue, plus the semantic corners
// (clamping, overlap, conflict-block straddle, marker handling, byte-
// identical fallback for empty masks).
// ---------------------------------------------------------------------------

/// Use case 1 — user edits a non-secret static field on a templated file
/// containing a secret. The deployed secret value also "rotated" so the
/// raw deployed line differs from the cached render. With the secret
/// line masked, only the static-text edit on the non-secret line should
/// land in the template diff. Critically, the resolved (rotated or
/// otherwise) secret value must NOT appear in the diff.
#[test]
fn mask_use_case_1_user_edit_non_secret_with_vault_rotation() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://DB/password".to_string(),
        "OLD-secret".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = r#"host = "localhost"
password = "{{ secret('op://DB/password') }}"
"#;
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    // User renamed `host` → `hostname` (static-text edit on line 0) AND
    // the deployed secret value happens to be the rotated value
    // "ROTATED-NEW-secret" (line 1).
    let deployed = "hostname = \"localhost\"\npassword = \"ROTATED-NEW-secret\"\n";
    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);

    assert!(
        diff.contains("-host = \"localhost\""),
        "expected removal of original host line; got: {diff}"
    );
    assert!(
        diff.contains("+hostname = \"localhost\""),
        "expected addition of renamed host line; got: {diff}"
    );
    assert!(
        !diff.contains("ROTATED-NEW-secret"),
        "rotated secret leaked into diff: {diff}"
    );
    assert!(
        !diff.contains("OLD-secret"),
        "cached secret leaked into diff: {diff}"
    );
}

/// Use case 2 — vault rotation alone, no user edits. The deployed file's
/// secret line shows the new vault value; the cached render shows the
/// old one. With the secret line masked, the diff is empty (no
/// template-space change to make).
#[test]
fn mask_use_case_2_vault_rotation_only_yields_unchanged() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://GH/token".to_string(),
        "ghp_OLD".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "GH_TOKEN=\"{{ secret('op://GH/token') }}\"\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    // Only the secret value differs in deployed; everything else matches.
    let deployed = "GH_TOKEN=\"ghp_NEW_ROTATED\"\n";
    let markers = ConflictMarkers::default();
    let mask = [0..1];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);
    assert_eq!(diff, "", "vault-rotation-only with mask must be Unchanged");
}

/// Use case 2b — same scenario as above but the cached render and
/// deployed agree on the secret value; the mask is still set. Diff
/// should still be empty (mask is a permission, not a forced change).
#[test]
fn mask_with_no_actual_change_in_masked_range_is_still_unchanged() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://GH/token".to_string(),
        "ghp_X".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "GH_TOKEN=\"{{ secret('op://GH/token') }}\"\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let deployed = "GH_TOKEN=\"ghp_X\"\n";
    let markers = ConflictMarkers::default();
    let mask = [0..1];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);
    assert_eq!(diff, "");
}

/// Use case 3 — user fully rewrote the file. Some edits land inside the
/// mask, others outside. burgertocow should diff the outside-mask edits
/// normally and omit the inside-mask ones.
#[test]
fn mask_use_case_3_partial_overlap_keeps_outside_edits() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://DB/password".to_string(),
        "s3cret".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = r#"# database config
host = "localhost"
password = "{{ secret('op://DB/password') }}"
port = 5432
"#;
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    // Lines (0-based):
    //   0: "# database config\n"          ← user edited (outside mask)
    //   1: "host = \"localhost\"\n"        ← unchanged
    //   2: "password = \"...\"\n"           ← user edited inside mask
    //   3: "port = 5432\n"                 ← user edited (outside mask)
    let deployed = "# DATABASE configuration\nhost = \"localhost\"\npassword = \"USER-EDITED-VALUE\"\nport = 6543\n";
    let markers = ConflictMarkers::default();
    let mask = [2..3];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);
    assert!(
        diff.contains("-# database config"),
        "comment-line removal missing: {diff}"
    );
    assert!(
        diff.contains("+# DATABASE configuration"),
        "comment-line addition missing: {diff}"
    );
    assert!(
        diff.contains("-port = 5432"),
        "port removal missing: {diff}"
    );
    assert!(
        diff.contains("+port = 6543"),
        "port addition missing: {diff}"
    );
    assert!(
        !diff.contains("USER-EDITED-VALUE"),
        "masked user value leaked: {diff}"
    );
    assert!(!diff.contains("s3cret"), "cached secret leaked: {diff}");
}

/// Use case 3b — only-inside-mask edits should resolve to Unchanged
/// even when other lines differ in deployed but those differences are
/// pure-data (variable-only) changes.
#[test]
fn mask_only_inside_mask_edits_yields_unchanged() {
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://DB/password".to_string(),
        "OLD".to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "host = \"localhost\"\npassword = \"{{ secret('op://DB/password') }}\"\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let deployed = "host = \"localhost\"\npassword = \"USER-EDIT\"\n";
    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert_eq!(diff, "", "in-mask-only edit must be Unchanged");
}

/// Use case 4 — future-proofing: the mask isn't tied to any secret
/// concept. A timestamp banner or a machine-specific override line can be
/// masked the same way.
#[test]
fn mask_use_case_4_machine_specific_override_line() {
    let mut tracker = Tracker::new();
    let src = "name = {{ name }}\nmachine_id = {{ mid }}\nversion = {{ ver }}\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render(
            "t",
            serde_json::json!({"name": "svc", "mid": "host-A", "ver": "1.0"}),
        )
        .unwrap();

    // Deployed file has a different machine_id (the machine-specific
    // override) and a different version (a real edit). Mask the
    // machine_id line.
    let deployed = "name = svc\nmachine_id = host-Z\nversion = 2.0\n";
    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    // version edit is a pure variable swap → silent.
    // machine_id edit is masked → silent.
    assert_eq!(diff, "");
}

/// Empty-mask byte-identity backstop — covers all four use cases above
/// at once: with no mask, the new entry point must produce exactly the
/// same string as the legacy `generate_diff_with_markers`. This is the
/// regression test the issue calls out for backward compatibility.
#[test]
fn mask_empty_is_byte_identical_to_legacy_for_many_inputs() {
    // A handful of diverse scenarios. For each, run with empty mask via
    // the new entry point and via the legacy entry point and assert
    // byte-equality.
    struct Case {
        template: &'static str,
        ctx: serde_json::Value,
        deployed: &'static str,
    }
    let cases = [
        Case {
            template: "Hello {{ u }}!\nBye.",
            ctx: serde_json::json!({"u": "Ada"}),
            deployed: "Hello Ada!\nBye for now.",
        },
        Case {
            template: "{% for i in items %}- {{ i }}\n{% endfor %}",
            ctx: serde_json::json!({"items": ["A", "B"]}),
            deployed: "- A\n- B\n",
        },
        Case {
            template: "{% for i in items %}- {{ i }}\n{% endfor %}",
            ctx: serde_json::json!({"items": ["A", "B"]}),
            deployed: "* A\n! B\n",
        },
        Case {
            template: "host = {{ h }}\nport = 80\n",
            ctx: serde_json::json!({"h": "localhost"}),
            deployed: "host = localhost\nport = 8080\n",
        },
        Case {
            template: "日本: {{ u }}",
            ctx: serde_json::json!({"u": "Ada"}),
            deployed: "World: Ada",
        },
    ];

    for (i, c) in cases.iter().enumerate() {
        let mut tracker = Tracker::new();
        tracker.add_template("t", c.template).unwrap();
        let tracked = tracker.render("t", &c.ctx).unwrap();
        let markers = ConflictMarkers::default();
        let legacy = generate_diff_with_markers(c.template, &tracked, c.deployed, &markers);
        let opts_diff = generate_diff_with_markers_opts(
            c.template,
            &tracked,
            c.deployed,
            &DiffOptions::new(&markers),
        );
        assert_eq!(
            legacy, opts_diff,
            "case {i}: empty mask diverged from legacy entry"
        );
    }
}

/// Out-of-bounds masked ranges must clamp silently rather than panic,
/// and an entirely-OOB mask must produce the same output as no mask at
/// all. Useful when the sidecar trails off the end of a re-rendered
/// file.
#[test]
fn mask_out_of_bounds_ranges_clamp_without_panic() {
    let mut tracker = Tracker::new();
    let src = "name = {{ n }}\nport = {{ p }}\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"n": "svc", "p": 80}))
        .unwrap();

    let deployed = "name = svc\nport = 8080\n";
    let markers = ConflictMarkers::default();
    let baseline = generate_diff_with_markers(src, &tracked, deployed, &markers);

    // Wildly out-of-bounds: file has 2 lines, mask points at lines
    // 100..1000 and at usize::MAX..usize::MAX.
    let mask = [100..1000, usize::MAX..usize::MAX];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);
    assert_eq!(
        diff, baseline,
        "fully OOB mask must be a no-op (clamped to empty)"
    );
}

/// A mask range that partially overhangs EOF — the in-bounds portion is
/// honoured; the overhang is clamped.
#[test]
fn mask_partial_overhang_clamps_to_eof() {
    let mut tracker = Tracker::new();
    let src = "a\nb = {{ b }}\nc\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({"b": "B"})).unwrap();

    // Deployed has 3 lines. Mask 1..50 → clamped to 1..3 → masks lines
    // "b = B\n" and "c\n". User changed line 2 ("c" → "Z").
    let deployed = "a\nb = B\nZ\n";
    let markers = ConflictMarkers::default();
    let mask = [1..50];
    let opts = DiffOptions::new(&markers).with_mask(&mask);

    let diff = generate_diff_with_markers_opts(src, &tracked, deployed, &opts);
    assert_eq!(diff, "", "all changes inside clamped mask → Unchanged");
}

/// Overlapping ranges in the mask are merged; the result is the same as
/// passing the union of the ranges.
#[test]
fn mask_overlapping_ranges_match_union() {
    let mut tracker = Tracker::new();
    let src = "a\nb\nc\nd\ne\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let deployed = "a\nB\nC\nD\ne\n";
    let markers = ConflictMarkers::default();
    let overlapping = [1..3, 2..4];
    let union = [1..4];

    let with_overlap = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&overlapping),
    );
    let with_union = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&union),
    );
    assert_eq!(with_overlap, with_union);
    // And both yield "Unchanged" because the union covers all edits.
    assert_eq!(with_union, "");
}

/// Masking a region that would otherwise produce a conflict block makes
/// the conflict disappear. Covers semantic corner #3 from the issue.
#[test]
fn mask_inside_conflict_block_drops_the_conflict() {
    // Two iterations edited differently → without mask this is a
    // conflict block. Mask one of the deployed lines and the conflict
    // either collapses to a clean replacement (the surviving iteration
    // becomes a single consistent loop edit) or to Unchanged (if the
    // masked iteration was the only divergent one).
    let mut tracker = Tracker::new();
    let src = "{% for i in items %}- {{ i }}\n{% endfor %}";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"items": ["A", "B"]}))
        .unwrap();

    // Deployed: line 0 "* A\n", line 1 "! B\n" — inconsistent prefixes.
    let deployed = "* A\n! B\n";

    // Sanity: without mask, this is a conflict.
    let markers = ConflictMarkers::default();
    let baseline = generate_diff_with_markers(src, &tracked, deployed, &markers);
    assert!(
        baseline.contains("diff decision needed"),
        "expected baseline conflict; got: {baseline}"
    );

    // With line 1 masked, the only remaining edit is "- A" → "* A",
    // which is a single consistent loop-body edit → no conflict.
    let mask = [1..2];
    let masked_diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert!(
        !masked_diff.contains("diff decision needed"),
        "mask should have eliminated the conflict; got: {masked_diff}"
    );
    assert!(
        masked_diff.contains("- {{ i }}") && masked_diff.contains("* {{ i }}"),
        "expected the surviving prefix flip to land in the diff; got: {masked_diff}"
    );
}

/// Masking the entire conflict region drops the whole block — Unchanged.
#[test]
fn mask_entire_conflict_block_yields_unchanged() {
    let mut tracker = Tracker::new();
    let src = "{% for i in items %}- {{ i }}\n{% endfor %}";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"items": ["A", "B"]}))
        .unwrap();

    let deployed = "* A\n! B\n";
    let markers = ConflictMarkers::default();
    let mask = [0..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert_eq!(diff, "");
}

/// Masking a multi-line secret block (PEM-style). When the rotated
/// secret has the same line count as the cached one — the typical case
/// for vault rotations of fixed-format keys, and what dodot guarantees
/// by regenerating the sidecar after each render — the entire span is
/// substituted and the diff is Unchanged.
#[test]
fn mask_multi_line_secret_block_yields_unchanged_on_rotation() {
    let old_pem = "-----BEGIN KEY-----\nold-line-1\nold-line-2\n-----END KEY-----";
    let new_pem = "-----BEGIN KEY-----\nNEW-line-1\nNEW-line-2\n-----END KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        old_pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    // Render layout (6 lines):
    //   0: "KEY<<EOF\n"
    //   1..5: the 4 lines of old_pem
    //   5: "EOF\n"
    let deployed = format!("KEY<<EOF\n{new_pem}\nEOF\n");
    let markers = ConflictMarkers::default();
    // Mask all the rendered PEM lines (1..5).
    let mask = [1..5];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        &deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert_eq!(
        diff, "",
        "multi-line secret rotation under mask must be Unchanged; got: {diff}"
    );
    // The rotated value must not have leaked anywhere.
    assert!(!diff.contains("NEW-line-1"));
    assert!(!diff.contains("NEW-line-2"));
}

/// Stale-sidecar safety net — when a masked deployed-line index points
/// past the end of the cached render (the render shrank but the
/// sidecar didn't catch up), the deployed line stays in the rebuilt
/// stream rather than being silently dropped. The reverse diff then
/// surfaces the unmatched edit so the caller can investigate, instead
/// of returning a misleading empty diff.
///
/// This is the contract Copilot review on PR #14 pointed out, and the
/// reason `apply_deployed_mask` does not drop unmatched lines.
#[test]
fn mask_with_stale_sidecar_surfaces_unmatched_deployed_lines() {
    let mut tracker = Tracker::new();
    // Render is 2 lines. Sidecar (mask) was generated against an older
    // 4-line render and still points at lines 2..3.
    let src = "name = {{ n }}\nrole = {{ r }}\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"n": "svc", "r": "primary"}))
        .unwrap();

    // Deployed has 4 lines — extra trailing content the user actually
    // wrote. The stale sidecar masks the third line (index 2), but
    // pure_render only has 2 lines, so the third deployed line has no
    // counterpart in the render.
    let deployed = "name = svc\nrole = primary\n# user added comment\nextra-line\n";
    let markers = ConflictMarkers::default();
    let mask = [2..3];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    // The unmatched deployed line must surface in the diff (either as
    // a unified-diff addition or as a conflict block) — it must NOT
    // silently disappear.
    assert!(
        !diff.is_empty(),
        "stale-sidecar mask must not produce a misleading empty diff"
    );
    assert!(
        diff.contains("user added comment") || diff.contains("extra-line"),
        "expected the unmatched deployed content to surface; got: {diff}"
    );
}

/// Multi-line secret rotation where the new value has FEWER lines than
/// the old. The mask covers the deployed lines that do exist; the
/// "missing" rendered lines fall outside the mask and surface as a
/// real template-space change (a deletion). Documents the contract:
/// dodot is responsible for keeping the sidecar in sync with the
/// current deployed file structure.
#[test]
fn mask_multi_line_secret_with_shrunken_value_surfaces_excess_lines() {
    let old_pem = "-----BEGIN KEY-----\nL1\nL2\nL3\n-----END KEY-----";
    // New value is shorter by one line.
    let new_pem = "-----BEGIN KEY-----\nL1-NEW\nL2-NEW\n-----END KEY-----";
    let values = Arc::new(Mutex::new(HashMap::from([(
        "op://SSH/key".to_string(),
        old_pem.to_string(),
    )])));
    let mut tracker = make_secret_tracker(values);
    let src = "KEY<<EOF\n{{ secret('op://SSH/key') }}\nEOF\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let deployed = format!("KEY<<EOF\n{new_pem}\nEOF\n");
    let markers = ConflictMarkers::default();
    // dodot regenerated the sidecar against the new render — so the
    // mask reflects the (shorter) deployed file's secret span.
    let mask = [1..4];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        &deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    // The rotated secret value must NEVER leak.
    assert!(!diff.contains("L1-NEW"), "rotated value leaked: {diff}");
    assert!(!diff.contains("L2-NEW"), "rotated value leaked: {diff}");
}

/// The masking decision is made on deployed-line index, not on whether
/// the corresponding tracked content carries `VAR_START` / `VAR_END`
/// markers. Verifies semantic corner #5 from the issue.
#[test]
fn mask_decision_uses_line_index_not_marker_presence() {
    let mut tracker = Tracker::new();
    // Three lines: line 0 has a variable, line 1 is pure static, line 2
    // has a variable. Mask line 1 (pure static — no markers in tracked
    // there).
    let src = "first = {{ a }}\nstatic-line\nthird = {{ c }}\n";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"a": "A", "c": "C"}))
        .unwrap();

    // Deployed: user edited line 1 (the static line — that's the one
    // we're masking). Line 0 and 2 unchanged.
    let deployed = "first = A\nLine the user edited\nthird = C\n";
    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert_eq!(
        diff, "",
        "static-line mask must apply just like variable-line mask"
    );
}

/// Sanity: masking does not perturb the tracker's marker accounting.
/// The `tracked` stream we pass into the diff pipeline is unchanged by
/// masking; only the deployed bytes are rewritten internally.
#[test]
fn mask_does_not_touch_tracker_marker_balance() {
    let mut tracker = Tracker::new();
    tracker
        .add_template("t", "x = {{ a }}\ny = {{ b }}\nz = {{ c }}\n")
        .unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"a": "A", "b": "B", "c": "C"}))
        .unwrap();

    let starts_before = tracked.tracked().matches(VAR_START).count();
    let ends_before = tracked.tracked().matches(VAR_END).count();

    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let _ = generate_diff_with_markers_opts(
        "x = {{ a }}\ny = {{ b }}\nz = {{ c }}\n",
        &tracked,
        "x = A\ny = X\nz = C\n",
        &DiffOptions::new(&markers).with_mask(&mask),
    );

    assert_eq!(tracked.tracked().matches(VAR_START).count(), starts_before);
    assert_eq!(tracked.tracked().matches(VAR_END).count(), ends_before);
}

/// Empty `mask_deployed_lines` slice and a non-empty slice that clamps
/// down to nothing must produce identical output to the legacy entry.
#[test]
fn mask_empty_after_clamp_matches_empty_mask() {
    let mut tracker = Tracker::new();
    tracker.add_template("t", "Hi\nBye.\n").unwrap();
    let tracked = tracker.render("t", serde_json::json!({})).unwrap();

    let deployed = "Hi\nGoodbye.\n";
    let markers = ConflictMarkers::default();
    let legacy = generate_diff_with_markers("Hi\nBye.\n", &tracked, deployed, &markers);

    // Mask is non-empty but every range is OOB → after clamping, no
    // lines are masked → behaviour must match the legacy call.
    let oob_only = [50..60, 100..200];
    let opts_diff = generate_diff_with_markers_opts(
        "Hi\nBye.\n",
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&oob_only),
    );
    assert_eq!(legacy, opts_diff);

    // Mask with collapsed ranges (start == end) must also match legacy.
    let zero_width = [0..0, 1..1];
    let opts_zw = generate_diff_with_markers_opts(
        "Hi\nBye.\n",
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&zero_width),
    );
    assert_eq!(legacy, opts_zw);
}

/// Mask with the deployed file lacking a trailing newline. The masked
/// substitution must respect the rendered line's newline state, and the
/// surrounding lines must still align.
#[test]
fn mask_handles_deployed_without_trailing_newline() {
    let mut tracker = Tracker::new();
    let src = "a\nb = {{ b }}";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker.render("t", serde_json::json!({"b": "B"})).unwrap();

    // Deployed has a different value on line 1 and no trailing newline,
    // matching pure exactly. Mask line 1.
    let deployed = "a\nb = B";
    let markers = ConflictMarkers::default();
    let mask = [1..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&markers).with_mask(&mask),
    );
    assert_eq!(diff, "");
}

/// Custom conflict markers compose with masking — when masking shrinks
/// a conflict region, the surviving conflict (if any) still uses the
/// caller's markers.
#[test]
fn mask_preserves_custom_conflict_markers() {
    let mut tracker = Tracker::new();
    let src = "{% for i in items %}- {{ i }}\n{% endfor %}";
    tracker.add_template("t", src).unwrap();
    let tracked = tracker
        .render("t", serde_json::json!({"items": ["A", "B", "C"]}))
        .unwrap();

    // Three iterations; lines 0 and 2 disagree, line 1 is masked.
    let deployed = "* A\n! B\n# C\n";
    let custom = ConflictMarkers::new("BEGIN-CONFLICT\n", "MID\n", "END-CONFLICT\n");
    let mask = [1..2];
    let diff = generate_diff_with_markers_opts(
        src,
        &tracked,
        deployed,
        &DiffOptions::new(&custom).with_mask(&mask),
    );
    if diff.contains("BEGIN-CONFLICT") {
        // If a conflict survives, it must use the custom markers.
        assert!(diff.contains("MID"));
        assert!(diff.contains("END-CONFLICT"));
        assert!(!diff.contains("diff decision needed"));
    }
    // The masked iteration's payload must not appear in the diff body.
    assert!(
        !diff.contains("! B"),
        "masked deployed payload leaked: {diff}"
    );
}
