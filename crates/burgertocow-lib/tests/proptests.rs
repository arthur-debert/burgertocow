//! Property-based tests for the reverse-templating pipeline.
//!
//! These tests focus on invariants that should hold for *any* well-formed
//! template / context / modification triple, rather than specific
//! expected-output comparisons. They're coarse (generating complete
//! minijinja templates with full operator coverage under proptest is
//! difficult) but each property catches a real class of regression.

use burgertocow::{generate_diff, Tracker};
use proptest::prelude::*;
use serde_json::json;

/// Generate a "static text" string that can appear around template tags.
/// We use letters/symbols that are disjoint from [`var_value`]'s alphabet
/// so char-level Myers can unambiguously distinguish "static" from
/// "variable value" regions when we swap a variable's value.
fn static_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('b'),
            Just('c'),
            Just(' '),
            Just('\n'),
            Just('.'),
            Just(','),
            Just('!'),
            Just('日'),
            Just('🦀'),
        ],
        0..30,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Generate a short variable name using a stable alphabet.
fn var_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("user".to_string()),
        Just("name".to_string()),
        Just("role".to_string()),
        Just("x".to_string()),
    ]
}

/// Variable values are generated from two disjoint alphabets so the
/// property tests can produce a guaranteed-clean swap: the "old" pool
/// uses digits 0–4, the "new" pool uses digits 5–9. Because the two
/// values share no characters, char-level Myers cannot find any common
/// subsequence inside the variable regions and will emit a single
/// Replace op spanning the whole old value — which lets
/// [`is_purely_variable`] correctly classify the change as data-only.
///
/// Additionally the digit alphabet is disjoint from [`static_text`] and
/// [`separator`] so static regions never match variable chars either.
fn var_value_old() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![Just('0'), Just('1'), Just('2'), Just('3'), Just('4')],
        1..12,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn var_value_new() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![Just('5'), Just('6'), Just('7'), Just('8'), Just('9')],
        1..12,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// A "static separator" — like `static_text()` but guaranteed to be
/// non-empty *and* to include at least one char that isn't part of a
/// variable value alphabet. That way, two variables in a template are
/// always separated by text we can unambiguously identify as "template",
/// which is the realistic shape of actual templates. The ambiguous
/// case (adjacent `{{a}}{{b}}` with no separator) is covered by
/// dedicated unit tests.
fn separator() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(" | ".to_string()),
        Just(", ".to_string()),
        Just("\n---\n".to_string()),
        Just(": ".to_string()),
    ]
}

/// Build a template of the form `{prefix}{{ var }}{separator}{{ var }}{suffix}`
/// along with matching context. We intentionally use the *same* variable
/// twice so a single `ctx` update changes both occurrences — that lets
/// the "pure variable change" invariant exercise multi-occurrence paths.
fn template_and_ctx() -> impl Strategy<Value = (String, String, serde_json::Value)> {
    (
        static_text(),
        separator(),
        static_text(),
        var_name(),
        var_value_old(),
    )
        .prop_map(|(prefix, middle, suffix, name, val)| {
            let tmpl = format!("{prefix}{{{{ {name} }}}}{middle}{{{{ {name} }}}}{suffix}");
            let ctx = json!({ name.clone(): val });
            (tmpl, name, ctx)
        })
}

fn render(template: &str, ctx: &serde_json::Value) -> Option<burgertocow::TrackedRender> {
    let mut t = Tracker::new();
    t.add_template("t", template).ok()?;
    t.render("t", ctx).ok()
}

proptest! {
    // An unmodified render must produce an empty diff. This is the
    // strongest round-trip property: if the user didn't change anything
    // we must detect zero template changes.
    #[test]
    fn unchanged_render_yields_empty_diff((template, _, ctx) in template_and_ctx()) {
        let Some(r) = render(&template, &ctx) else {
            return Ok(());
        };
        let d = generate_diff(&template, &r, r.output());
        prop_assert_eq!(d, "");
    }

    // Swapping the variable to a new value in the context must also
    // produce an empty diff: a pure-data change is silent.
    #[test]
    fn pure_variable_swap_yields_empty_diff(
        (template, name, ctx) in template_and_ctx(),
        new_val in var_value_new(),
    ) {
        let Some(r) = render(&template, &ctx) else { return Ok(()); };
        let new_ctx = json!({ name: new_val });
        // Render with new context to get a "modified" output that is
        // what the plain render would look like under the new data.
        let Some(r2) = render(&template, &new_ctx) else { return Ok(()); };
        // Skip cases where the new render happens to *equal* the old —
        // nothing to assert there.
        if r.output() == r2.output() {
            return Ok(());
        }
        let d = generate_diff(&template, &r, r2.output());
        prop_assert_eq!(d, "");
    }

    // `generate_diff` must never panic, even on adversarial modified
    // text. This is a rough-edges check: crashes in the char/byte
    // boundary logic have shown up there before.
    #[test]
    fn generate_diff_never_panics(
        (template, _, ctx) in template_and_ctx(),
        modified in static_text(),
    ) {
        let Some(r) = render(&template, &ctx) else { return Ok(()); };
        let _ = generate_diff(&template, &r, &modified);
    }

    // A static-text edit produces a non-empty diff (either a unified
    // diff or a conflict block). Together with the empty-diff
    // properties above, this asserts that we don't accidentally drop
    // template changes as pure-variable.
    #[test]
    fn static_text_edit_is_detected(
        (template, _, ctx) in template_and_ctx(),
        injected in prop_oneof![
            Just(" EXTRA".to_string()),
            Just("!!!".to_string()),
            Just("# note".to_string()),
        ],
    ) {
        let Some(r) = render(&template, &ctx) else { return Ok(()); };
        // Append the extra text *after* the rendered output. If the
        // template ended with a variable, the append is at a boundary
        // and the algorithm conservatively treats it as a template
        // edit. Either way, the diff must not be empty.
        let modified = format!("{}{}", r.output(), injected);
        if modified == r.output() { return Ok(()); }
        let d = generate_diff(&template, &r, &modified);
        prop_assert!(!d.is_empty(), "expected non-empty diff for static edit, got empty");
    }

    // Marker-sanitization invariant: even if the variable value
    // contains the marker bytes, `tracked()` must still have a balanced
    // count of start/end markers equal to the number of variable
    // emissions.
    #[test]
    fn markers_stay_balanced_even_with_hostile_values(
        prefix in static_text(),
        hostile in prop::string::string_regex("[\u{1E}\u{1F}a-z]{0,30}").unwrap(),
    ) {
        let template = format!("{prefix}{{{{ v }}}}");
        let Some(r) = render(&template, &json!({"v": hostile})) else { return Ok(()); };
        let starts = r.tracked().matches('\u{1E}').count();
        let ends = r.tracked().matches('\u{1F}').count();
        prop_assert_eq!(starts, 1);
        prop_assert_eq!(ends, 1);
    }
}
