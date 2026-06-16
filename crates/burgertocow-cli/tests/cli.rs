//! End-to-end tests for the `burgertocow` CLI binary.
//!
//! These exercise the actual built binary (via the `CARGO_BIN_EXE_burgertocow`
//! env var that cargo sets for integration tests). They lock in the
//! contract the CLI exposes to humans and scripts:
//!
//! * `render` writes the marker-stripped output to the requested file
//!   and reports the destination on stdout,
//! * `diff` prints a template-space diff for template edits and an
//!   empty string for pure-variable edits,
//! * the CLI surfaces user-visible context for missing-file and bad-
//!   JSON errors instead of swallowing them.
//!
//! These are not duplicates of the library integration tests in
//! `crates/burgertocow-lib/tests/integration.rs` — those test the
//! library API in-process; this file tests the binary's argument
//! parsing, file I/O, stdout/stderr surface, and exit codes.
//!
//! Repo fixtures under `../../tests/fixtures/` are reused so we don't
//! re-stage template/data files just for the CLI.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_burgertocow")
}

fn fixtures_dir() -> PathBuf {
    // Each integration-test file is compiled with CARGO_MANIFEST_DIR set
    // to its crate's directory (crates/burgertocow-cli). The fixtures
    // live at the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn fixture(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

/// Per-test scratch directory. We don't take a dev-dep on `tempfile`
/// just for this — a deterministic per-test path under `target/` is
/// enough, and cargo cleans it up with `cargo clean`.
fn scratch_dir(test_name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test_name);
    // Wipe any leftovers from a previous run so assertions never see
    // stale files.
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("failed to spawn burgertocow binary")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout was not UTF-8")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr was not UTF-8")
}

fn fixture_str(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap()
}

fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, got status={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout_of(out),
        stderr_of(out)
    );
}

fn assert_failure(out: &Output) {
    assert!(
        !out.status.success(),
        "expected failure, got status={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout_of(out),
        stderr_of(out)
    );
}

fn template_arg(name: &str) -> String {
    fixture(name).to_string_lossy().into_owned()
}

fn out_arg(dir: &Path, name: &str) -> String {
    dir.join(name).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------
// render

#[test]
fn render_writes_marker_stripped_output_to_file() {
    let dir = scratch_dir("render_writes_output");
    let out_path = dir.join("out.md");

    let out = run(&[
        "render",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--out",
        &out_arg(&dir, "out.md"),
    ]);
    assert_success(&out);

    let written = std::fs::read_to_string(&out_path).expect("output file missing");
    // simple.md is "Hello {{ user }}!\nWelcome to the template.\n" and
    // simple_data.json puts user=Arthur, so the rendered file must
    // expand the variable and must NOT contain the boundary markers.
    assert_eq!(written, "Hello Arthur!\nWelcome to the template.\n");
    assert!(
        !written.contains('\u{001E}') && !written.contains('\u{001F}'),
        "rendered file leaked variable-boundary markers: {written:?}"
    );

    // CLI reports where it wrote the file, on stdout.
    let so = stdout_of(&out);
    assert!(
        so.contains(out_path.to_string_lossy().as_ref()),
        "stdout did not mention the output path: {so}"
    );
}

#[test]
fn render_expands_loop_template_to_concrete_iterations() {
    let dir = scratch_dir("render_loop_template");
    let out_path = dir.join("loop_out.md");

    let out = run(&[
        "render",
        "--template",
        &template_arg("loop.md"),
        "--data",
        &template_arg("loop_data.json"),
        "--out",
        &out_arg(&dir, "loop_out.md"),
    ]);
    assert_success(&out);

    let written = std::fs::read_to_string(&out_path).expect("output file missing");
    // loop.md iterates over ["Apple", "Banana"] — both must appear.
    assert!(written.contains("* Apple"), "Apple missing: {written:?}");
    assert!(written.contains("* Banana"), "Banana missing: {written:?}");
}

// ---------------------------------------------------------------------
// diff

#[test]
fn diff_pure_variable_change_emits_empty_output() {
    // simple_mod_var.md only changes the rendered value of {{ user }},
    // so the reverse-template diff must be empty.
    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--modified",
        &template_arg("simple_mod_var.md"),
    ]);
    assert_success(&out);
    assert_eq!(
        stdout_of(&out),
        "",
        "expected empty diff for pure-variable edit, got:\n{}",
        stdout_of(&out)
    );
    assert!(out.stderr.is_empty(), "unexpected stderr: {:?}", out.stderr);
}

#[test]
fn diff_template_text_change_emits_unified_diff() {
    // simple_mod_text.md changes static template text. The CLI should
    // print a non-empty unified-diff body capturing the static change.
    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--modified",
        &template_arg("simple_mod_text.md"),
    ]);
    assert_success(&out);

    let so = stdout_of(&out);
    assert!(!so.is_empty(), "expected a non-empty diff");
    assert!(
        so.contains("-Welcome to the template."),
        "missing removal line in diff:\n{so}"
    );
    assert!(
        so.contains("+Welcome to the modified template."),
        "missing addition line in diff:\n{so}"
    );
}

// ---------------------------------------------------------------------
// error paths

#[test]
fn diff_missing_template_reports_template_path() {
    // The CLI wraps the read-template error with a `reading template
    // <path>` context line via anyhow. Confirm that context survives to
    // stderr so a user sees which file was missing.
    let out = run(&[
        "diff",
        "--template",
        "/definitely/does/not/exist/template.md",
        "--data",
        &template_arg("simple_data.json"),
        "--modified",
        &template_arg("simple_mod_var.md"),
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains("reading template"),
        "stderr missing template-read context:\n{se}"
    );
    assert!(
        se.contains("/definitely/does/not/exist/template.md"),
        "stderr missing the offending path:\n{se}"
    );
}

#[test]
fn diff_missing_data_reports_data_path() {
    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        "/definitely/does/not/exist/data.json",
        "--modified",
        &template_arg("simple_mod_var.md"),
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains("reading data"),
        "stderr missing data-read context:\n{se}"
    );
}

#[test]
fn diff_missing_modified_reports_modified_path() {
    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--modified",
        "/definitely/does/not/exist/modified.md",
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains("reading modified"),
        "stderr missing modified-read context:\n{se}"
    );
}

#[test]
fn render_missing_template_reports_template_path() {
    let dir = scratch_dir("render_missing_template");
    let out = run(&[
        "render",
        "--template",
        "/definitely/does/not/exist/template.md",
        "--data",
        &template_arg("simple_data.json"),
        "--out",
        &out_arg(&dir, "out.md"),
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains("reading template"),
        "stderr missing template-read context:\n{se}"
    );
}

#[test]
fn render_invalid_json_data_reports_data_path() {
    // Write a deliberately malformed JSON file and feed it to `render`.
    // We expect the CLI's error message to identify which file failed
    // to parse — otherwise the user has to guess between --template
    // and --data on a JSON-parse failure. This is the bug the test
    // locks down (see fix in src/main.rs: the serde_json::from_str
    // call now carries a `with_context(...)` data-file note).
    let dir = scratch_dir("render_invalid_json");
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{ this is not valid json").unwrap();

    let out = run(&[
        "render",
        "--template",
        &template_arg("simple.md"),
        "--data",
        bad.to_string_lossy().as_ref(),
        "--out",
        &out_arg(&dir, "out.md"),
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains(bad.to_string_lossy().as_ref()),
        "stderr did not identify the bad data file: {se}"
    );
}

#[test]
fn diff_invalid_json_data_reports_data_path() {
    let dir = scratch_dir("diff_invalid_json");
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{not-json").unwrap();

    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        bad.to_string_lossy().as_ref(),
        "--modified",
        &template_arg("simple_mod_text.md"),
    ]);
    assert_failure(&out);
    let se = stderr_of(&out);
    assert!(
        se.contains(bad.to_string_lossy().as_ref()),
        "stderr did not identify the bad data file: {se}"
    );
}

// ---------------------------------------------------------------------
// arg parsing

#[test]
fn missing_subcommand_exits_nonzero_with_usage() {
    let out = run(&[]);
    assert_failure(&out);
    // clap prints usage to stderr on missing required subcommand.
    let se = stderr_of(&out);
    assert!(
        se.contains("Usage:") || se.to_lowercase().contains("usage"),
        "expected a usage hint on missing subcommand: {se}"
    );
}

#[test]
fn help_lists_both_subcommands() {
    let out = run(&["--help"]);
    assert_success(&out);
    let so = stdout_of(&out);
    assert!(so.contains("render"), "help missing `render`: {so}");
    assert!(so.contains("diff"), "help missing `diff`: {so}");
}

#[test]
fn version_flag_prints_a_version_line() {
    let out = run(&["--version"]);
    assert_success(&out);
    let so = stdout_of(&out);
    // clap's default --version output is "<bin> <version>". We only
    // assert the bin name; the version itself moves every release.
    assert!(
        so.contains("burgertocow"),
        "expected --version to mention bin name: {so}"
    );
}

// ---------------------------------------------------------------------
// round-trip: render then diff against the rendered output

#[test]
fn render_then_diff_against_pristine_render_is_empty() {
    // Render simple.md to disk, then immediately diff the same render
    // back as the "modified" input. With no edits applied there must
    // be no template diff at all.
    let dir = scratch_dir("roundtrip");
    let out_path = dir.join("rendered.md");

    let out = run(&[
        "render",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--out",
        &out_arg(&dir, "rendered.md"),
    ]);
    assert_success(&out);

    let out = run(&[
        "diff",
        "--template",
        &template_arg("simple.md"),
        "--data",
        &template_arg("simple_data.json"),
        "--modified",
        out_path.to_string_lossy().as_ref(),
    ]);
    assert_success(&out);
    assert_eq!(
        stdout_of(&out),
        "",
        "roundtrip render→diff produced non-empty diff:\n{}",
        stdout_of(&out)
    );
}

#[test]
fn fixture_source_files_are_present() {
    // Belt-and-braces: if someone moves the workspace fixture dir, the
    // CLI integration suite must fail loudly rather than skip.
    for name in [
        "simple.md",
        "simple_data.json",
        "simple_mod_var.md",
        "simple_mod_text.md",
        "loop.md",
        "loop_data.json",
    ] {
        let p = fixture(name);
        assert!(
            p.exists(),
            "expected fixture {p:?} to exist; full content of simple.md = {:?}",
            std::fs::read_to_string(fixture("simple.md")).ok()
        );
    }
    // Sanity-check one read so a regression on encoding gets surfaced
    // here, not deep inside a CLI invocation.
    assert!(fixture_str("simple.md").contains("{{ user }}"));
}
