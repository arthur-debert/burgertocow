//! Reverse-engineer a diff on a rendered `minijinja` template back into
//! template-level and variable-level changes.
//!
//! # Overview
//!
//! Given a source template, a rendering context, and a downstream tool's
//! modified version of the rendered output, we want to answer: which of the
//! user's edits were changes to *template* text (static boilerplate) and
//! which were changes to *data* (variable values)?
//!
//! This crate implements the "shadow rendering + skeleton alignment"
//! heuristic outlined in the README.
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

pub mod diff;
pub mod engine;
pub mod parser;

pub use diff::{generate_diff, CONFLICT_END, CONFLICT_MID, CONFLICT_START};
pub use engine::{TrackedRender, Tracker, VAR_END, VAR_START};
