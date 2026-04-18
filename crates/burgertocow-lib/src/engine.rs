//! Tracked rendering on top of `minijinja`.
//!
//! `Tracker` wraps a `minijinja::Environment` with a formatter that wraps
//! every variable emission in invisible ASCII markers (`U+001E` / `U+001F`).
//! The public surface mirrors `minijinja::Environment` so swapping this in
//! for a plain minijinja setup is a local change: configure the environment
//! via `env_mut()`, render with `render(name, ctx)`.

use minijinja::{escape_formatter, value::Value, Environment, Error};
use serde::Serialize;

pub const VAR_START: char = '\x1E';
pub const VAR_END: char = '\x1F';

const VAR_START_STR: &str = "\x1E";
const VAR_END_STR: &str = "\x1F";

fn tracking_formatter(
    out: &mut minijinja::Output<'_>,
    state: &minijinja::State<'_, '_>,
    value: &Value,
) -> Result<(), Error> {
    out.write_str(VAR_START_STR)?;
    escape_formatter(out, state, value)?;
    out.write_str(VAR_END_STR)?;
    Ok(())
}

/// A minijinja `Environment` instrumented to produce tracked renders.
pub struct Tracker {
    env: Environment<'static>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
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
    pub fn env_mut(&mut self) -> &mut Environment<'static> {
        &mut self.env
    }

    /// Convenience wrapper around `Environment::add_template_owned`.
    pub fn add_template(&mut self, name: &str, source: &str) -> Result<(), Error> {
        self.env
            .add_template_owned(name.to_string(), source.to_string())
    }

    /// Render a registered template. Mirrors `Template::render` from minijinja
    /// but returns a `TrackedRender` carrying both the clean output and the
    /// tracked output with variable-boundary markers.
    pub fn render<S: Serialize>(&self, name: &str, ctx: S) -> Result<TrackedRender, Error> {
        let tmpl = self.env.get_template(name)?;
        let tracked = tmpl.render(ctx)?;
        Ok(TrackedRender::from_tracked(tracked))
    }
}

/// The output of a tracked render.
///
/// `output` is the user-visible rendered string (identical to what a plain
/// minijinja render would produce). `tracked` is the same string with
/// `VAR_START`/`VAR_END` markers around each variable's emission.
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

    /// The rendered output with tracking markers stripped.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// The rendered output with variable-boundary tracking markers retained.
    pub fn tracked(&self) -> &str {
        &self.tracked
    }

    /// Consume self and return (output, tracked).
    pub fn into_parts(self) -> (String, String) {
        (self.output, self.tracked)
    }
}
