//! Reverse-engineer a diff on a rendered `minijinja` template back into
//! template-level and variable-level changes.
//!
//! # Problem statement
//!
//! Given a **source template**, a **rendering context**, and a downstream
//! tool's **modified** version of the rendered output, which of the
//! user's edits were changes to the *template* (static boilerplate) and
//! which were changes to the *data* (variable values)?
//!
//! # Approach
//!
//! This crate implements the "shadow rendering + skeleton alignment"
//! heuristic detailed in the crate README. At the edges:
//!
//! * [`Tracker`] instruments `minijinja` to emit variable-boundary
//!   markers alongside the normal render, without changing the visible
//!   output.
//! * [`generate_diff`] consumes the template, the marker-carrying
//!   render, and the modified text, and returns either a unified diff
//!   expressed against the template, or a conflict block
//!   (`<<<< diff decision needed >>>>`) for edits it cannot safely
//!   attribute.
//!
//! Pure-data edits produce the empty string. Template-level edits
//! produce standard unified-diff text suitable for feeding to
//! `git apply` against the source template file.
//!
//! # Quick start
//!
//! ```no_run
//! use burgertocow::{generate_diff, Tracker};
//!
//! let mut tracker = Tracker::new();
//! tracker.add_template("greet", "Hello {{ user }}!\nWelcome.").unwrap();
//!
//! let ctx = serde_json::json!({ "user": "Arthur" });
//! let tracked = tracker.render("greet", &ctx).unwrap();
//!
//! // downstream tool produces a modified version of tracked.output()
//! let modified = "Hello Zaphod!\nWelcome, friend.";
//! let diff = generate_diff("Hello {{ user }}!\nWelcome.", &tracked, modified);
//! println!("{diff}");
//! ```
//!
//! # Known limitations
//!
//! * Char-level diff: variable values sharing characters with
//!   surrounding static text can fragment Myers alignment. Covered in
//!   the `diff` module docs.
//! * Adjacent variables with no separator (`{{a}}{{b}}`) and edits at
//!   variable boundaries are conservatively reported as template edits.
//! * Loop bodies that vary per iteration (e.g. `{% if loop.first %}`)
//!   bypass the loop-iteration fallback and surface as conflicts.

pub mod diff;
pub mod engine;
pub mod parser;

pub use diff::{generate_diff, CONFLICT_END, CONFLICT_MID, CONFLICT_START};
pub use engine::{TrackedRender, Tracker, VAR_END, VAR_START};
