//! Tracked rendering on top of `minijinja`.
//!
//! [`Tracker`] wraps a [`minijinja::Environment`] with a custom formatter that
//! wraps every variable emission in a pair of ASCII information-separator
//! control characters (`U+001E` = [`VAR_START`], `U+001F` = [`VAR_END`]).
//! The public surface mirrors [`minijinja::Environment`] so swapping this in
//! for a plain minijinja setup is a local change: configure the environment
//! via [`env_mut`](Tracker::env_mut), render with
//! [`render`](Tracker::render).
//!
//! # Why these markers
//!
//! `\x1E` and `\x1F` are non-printable ASCII control codes that essentially
//! never appear in natural content. Picking single-codepoint markers keeps
//! the tracked output close in size and char-offset to the clean render,
//! which simplifies downstream alignment math.
//!
//! # Marker collisions
//!
//! If a user-supplied variable value literally contains one of these two
//! bytes, a naive formatter would inject a spurious boundary and corrupt
//! the tracking. We defend against that for plain-text templates
//! ([`AutoEscape::None`]) by capturing the value's rendered form into a
//! temporary buffer and stripping any marker bytes before emitting.
//!
//! For HTML / JSON auto-escape modes we delegate to
//! [`escape_formatter`] — both escape schemes already produce marker-free
//! output for any normal input (HTML entity-encoding doesn't touch those
//! bytes, but JSON escapes them as `\u001e` / `\u001f`). The one residual
//! risk is an `AutoEscape::Html` template combined with a
//! [`Value::from_safe_string`] value whose raw bytes include a marker; that
//! is an intentionally unescaped passthrough and is documented here.

use minijinja::{escape_formatter, value::Value, AutoEscape, Environment, Error, ErrorKind};
use serde::Serialize;
use std::fmt::Write as _;

/// The character placed *before* every variable emission in
/// [`TrackedRender::tracked`].
pub const VAR_START: char = '\x1E';
/// The character placed *after* every variable emission in
/// [`TrackedRender::tracked`].
pub const VAR_END: char = '\x1F';

const VAR_START_STR: &str = "\x1E";
const VAR_END_STR: &str = "\x1F";

/// Custom formatter invoked by minijinja for every `{{ expr }}` emission.
///
/// Emits [`VAR_START`], the value, then [`VAR_END`]. For the plain-text
/// path the value is captured into a scratch buffer and marker characters
/// are stripped before being written to the real output — that keeps the
/// balance `#VAR_START == #VAR_END == #variables` invariant intact even
/// when callers pass untrusted data.
fn tracking_formatter(
    out: &mut minijinja::Output<'_>,
    state: &minijinja::State<'_, '_>,
    value: &Value,
) -> Result<(), Error> {
    out.write_str(VAR_START_STR)?;

    match state.auto_escape() {
        AutoEscape::None => {
            // Render the value with plain Display (equivalent to what
            // `escape_formatter` does for `AutoEscape::None`) into a local
            // buffer, strip marker bytes, then emit.
            let mut buf = String::new();
            write!(&mut buf, "{value}")
                .map_err(|_| Error::new(ErrorKind::WriteFailure, "formatter buffer"))?;
            if buf.contains(VAR_START) || buf.contains(VAR_END) {
                let sanitized: String = buf
                    .chars()
                    .filter(|&c| c != VAR_START && c != VAR_END)
                    .collect();
                out.write_str(&sanitized)?;
            } else {
                out.write_str(&buf)?;
            }
        }
        _ => {
            // HTML / JSON / custom: delegate. HTML entity-encoding and JSON
            // serialization both convert raw control bytes into safe
            // escape sequences, so markers cannot leak for normal values.
            escape_formatter(out, state, value)?;
        }
    }

    out.write_str(VAR_END_STR)?;
    Ok(())
}

/// A [`minijinja::Environment`] instrumented to produce tracked renders.
///
/// Configure filters, globals, loaders, autoescape, etc. via
/// [`env_mut`](Self::env_mut) exactly like you would on a raw
/// `Environment`. Then call [`render`](Self::render) to obtain a
/// [`TrackedRender`] that carries both the plain rendered string and the
/// marker-annotated twin.
pub struct Tracker {
    env: Environment<'static>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    /// Create a new tracker with an empty environment and the tracking
    /// formatter installed.
    ///
    /// `keep_trailing_newline` is enabled so that template files ending in
    /// `\n` round-trip faithfully — losing the final newline is a common
    /// source of spurious "template edit" diffs.
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.set_formatter(tracking_formatter);
        env.set_keep_trailing_newline(true);
        Self { env }
    }

    /// Borrow the underlying minijinja `Environment` (read-only).
    pub fn env(&self) -> &Environment<'static> {
        &self.env
    }

    /// Mutably borrow the underlying minijinja `Environment` so callers can
    /// register templates, filters, globals, etc. using the native API.
    ///
    /// Do **not** call `set_formatter` on the returned environment — it
    /// would replace the tracking formatter and break reverse-diffing.
    pub fn env_mut(&mut self) -> &mut Environment<'static> {
        &mut self.env
    }

    /// Convenience wrapper around `Environment::add_template_owned`.
    pub fn add_template(&mut self, name: &str, source: &str) -> Result<(), Error> {
        self.env
            .add_template_owned(name.to_string(), source.to_string())
    }

    /// Render a registered template and return a [`TrackedRender`] carrying
    /// both the clean output and the marker-annotated version.
    pub fn render<S: Serialize>(&self, name: &str, ctx: S) -> Result<TrackedRender, Error> {
        let tmpl = self.env.get_template(name)?;
        let tracked = tmpl.render(ctx)?;
        Ok(TrackedRender::from_tracked(tracked))
    }
}

/// The output of a tracked render.
///
/// * [`output`](Self::output) is the user-visible rendered string —
///   identical to what a plain `minijinja::Environment` with the same
///   configuration would produce.
/// * [`tracked`](Self::tracked) is the same string with [`VAR_START`] /
///   [`VAR_END`] markers wrapping every variable emission.
#[derive(Debug, Clone)]
pub struct TrackedRender {
    output: String,
    tracked: String,
}

impl TrackedRender {
    pub(crate) fn from_tracked(tracked: String) -> Self {
        let mut output = String::with_capacity(tracked.len());
        for c in tracked.chars() {
            if c != VAR_START && c != VAR_END {
                output.push(c);
            }
        }
        Self { output, tracked }
    }

    /// Rehydrate a `TrackedRender` from a previously-saved tracked string.
    ///
    /// Callers that persist the output of [`tracked`](Self::tracked) (for
    /// example, a cache that wants to invoke
    /// [`generate_diff`](crate::generate_diff) against a stored render
    /// without re-running the template) can rebuild the in-memory form via
    /// this constructor. The plain `output` is recomputed by stripping
    /// marker characters, so callers don't need to persist it separately.
    ///
    /// This is the inverse of [`tracked`](Self::tracked): for any
    /// `TrackedRender` produced by [`Tracker::render`],
    /// `TrackedRender::from_tracked_string(r.tracked().to_owned())` yields
    /// a value with the same `output()` and `tracked()`.
    ///
    /// The input is trusted — the constructor does not validate marker
    /// balance. A `tracked` string with mismatched [`VAR_START`] /
    /// [`VAR_END`] markers will produce a `TrackedRender` that still
    /// renders sensibly (markers are stripped) but whose use as input to
    /// the diff pipeline will yield ambiguous results. Cache layers
    /// should preserve the string verbatim from the original render.
    pub fn from_tracked_string(tracked: String) -> Self {
        Self::from_tracked(tracked)
    }

    /// The rendered output with tracking markers stripped.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// The rendered output with variable-boundary tracking markers retained.
    pub fn tracked(&self) -> &str {
        &self.tracked
    }

    /// Consume self and return `(output, tracked)`.
    pub fn into_parts(self) -> (String, String) {
        (self.output, self.tracked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markers_wrap_each_variable() {
        let mut t = Tracker::new();
        t.add_template("x", "Hello {{ name }}!").unwrap();
        let r = t.render("x", json!({"name": "Ada"})).unwrap();
        assert_eq!(r.output(), "Hello Ada!\n".trim_end_matches('\n'));
        // One start and one end per variable emission.
        assert_eq!(r.tracked().matches(VAR_START).count(), 1);
        assert_eq!(r.tracked().matches(VAR_END).count(), 1);
    }

    #[test]
    fn output_strips_markers_even_if_value_contained_them() {
        let mut t = Tracker::new();
        t.add_template("x", "<{{ payload }}>").unwrap();
        let r = t
            .render("x", json!({"payload": "A\u{1e}B\u{1f}C"}))
            .unwrap();
        // Sanitization removes marker bytes from the value before emission,
        // so the visible output no longer contains them at all.
        assert_eq!(r.output(), "<ABC>");
        // And the tracked stream has exactly one pair of markers.
        assert_eq!(r.tracked().matches(VAR_START).count(), 1);
        assert_eq!(r.tracked().matches(VAR_END).count(), 1);
    }

    #[test]
    fn output_equals_plain_minijinja_for_normal_values() {
        use minijinja::Environment;

        let src = "Hi {{ user }}, {{ items | length }} items";
        let mut plain = Environment::new();
        plain.set_keep_trailing_newline(true);
        plain
            .add_template_owned("t".to_string(), src.to_string())
            .unwrap();
        let ctx = json!({"user": "A", "items": [1, 2, 3]});
        let plain_out = plain.get_template("t").unwrap().render(&ctx).unwrap();

        let mut t = Tracker::new();
        t.add_template("t", src).unwrap();
        let tracked = t.render("t", &ctx).unwrap();

        assert_eq!(tracked.output(), plain_out);
    }

    #[test]
    fn loops_emit_one_marker_pair_per_iteration() {
        let mut t = Tracker::new();
        t.add_template("l", "{% for i in items %}{{ i }}{% endfor %}")
            .unwrap();
        let r = t.render("l", json!({"items": ["a", "b", "c"]})).unwrap();
        assert_eq!(r.output(), "abc");
        assert_eq!(r.tracked().matches(VAR_START).count(), 3);
        assert_eq!(r.tracked().matches(VAR_END).count(), 3);
    }

    #[test]
    fn from_tracked_string_round_trips_render() {
        // The use case: a downstream cache stores the tracked string from
        // an earlier render, then later wants to feed it into the diff
        // pipeline without re-rendering. Round-trip via the public
        // constructor must reproduce both `output` and `tracked` exactly.
        let mut t = Tracker::new();
        t.add_template("x", "Hello {{ name }}, {{ count }} items")
            .unwrap();
        let original = t.render("x", json!({"name": "Ada", "count": 3})).unwrap();

        let cached = original.tracked().to_owned();
        let rehydrated = TrackedRender::from_tracked_string(cached);

        assert_eq!(rehydrated.output(), original.output());
        assert_eq!(rehydrated.tracked(), original.tracked());
        // Marker count is preserved.
        assert_eq!(
            rehydrated.tracked().matches(VAR_START).count(),
            original.tracked().matches(VAR_START).count()
        );
    }

    #[test]
    fn from_tracked_string_drives_generate_diff() {
        // End-to-end: rehydrate from a cached tracked string and pass the
        // result into `generate_diff` — no re-render involved.
        use crate::generate_diff;

        let template = "name = {{ user }}\ncount = 1\n";
        let mut t = Tracker::new();
        t.add_template("c", template).unwrap();
        let original = t.render("c", json!({"user": "Ada"})).unwrap();

        let cached = original.tracked().to_owned();
        let rehydrated = TrackedRender::from_tracked_string(cached);

        // Static-line edit: should produce a non-empty diff against the
        // template (the rehydrated render is functionally equivalent).
        let modified = "name = Ada\ncount = 99\n";
        let diff_orig = generate_diff(template, &original, modified);
        let diff_rehydrated = generate_diff(template, &rehydrated, modified);
        assert_eq!(diff_orig, diff_rehydrated);
        assert!(!diff_rehydrated.is_empty());
    }

    #[test]
    fn empty_variable_emits_balanced_markers() {
        let mut t = Tracker::new();
        t.add_template("e", "[{{ x }}]").unwrap();
        let r = t.render("e", json!({"x": ""})).unwrap();
        assert_eq!(r.output(), "[]");
        // Empty variable still gets a start/end pair so parsers see it.
        assert!(r.tracked().contains("\x1e\x1f"));
    }

    #[test]
    fn unicode_value_passes_through_untouched() {
        let mut t = Tracker::new();
        t.add_template("u", "{{ greet }}").unwrap();
        let r = t.render("u", json!({"greet": "héllo 世界 🌍"})).unwrap();
        assert_eq!(r.output(), "héllo 世界 🌍");
    }
}
