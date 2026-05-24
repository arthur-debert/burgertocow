
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-05-02


### Added

- **`generate_diff_with_markers_opts` + `DiffOptions`** — new entry point
  that accepts a `DiffOptions` struct carrying the conflict markers and
  an optional list of deployed-file line ranges to mask out of the
  reverse-diff. Masked lines are treated as if they always matched the
  cached render, regardless of actual content. The motivating use case
  is dodot's `secret()` integration: lines populated from a vault must
  not participate in template-space diffing (otherwise a vault rotation
  or a hand-edited secret line would rewrite the template's
  `{{ secret(...) }}` expression to a literal value, defeating the
  abstraction). The mechanism is general — any deployed-file line whose
  source-of-truth lives outside the deployed bytes (machine overrides,
  timestamp banners, etc.) can be masked the same way. Out-of-bounds
  ranges clamp silently; overlapping ranges merge. The legacy
  `generate_diff_with_markers` is unchanged and remains a thin wrapper
  that builds an empty mask. (#13)
## [0.3.1] - 2026-05-01


### Changed

- **Release pipeline migrated to canonical reusable workflow at
  `arthur-debert/release/.github/workflows/rust-cli.yml@v1`.** burgertocow's
  `.github/workflows/release.yml` is now a thin caller. Fifth consumer
  of the new pipeline (after dodot v2.0.0, padz v1.8.2, simple-gal
  v0.20.4, rustloc v0.14.2 — all verified end-to-end). Bug fixes
  propagate via a single bump of the action's `@v1` ref.
- **Tarball naming + layout changed to canonical** (full Rust target
  triples + subdir layout). Brew formula handles both layouts.
- **Intel-mac dropped from release artifacts** (`x86_64-apple-darwin`).
  arm64-only macOS by canonical convention. v0.3.0 and earlier remain
  available for Intel users via direct GH release download.
## [0.3.0] - 2026-05-01

### Added

- **`TrackedRender::from_tracked_string`** — public constructor that rehydrates a `TrackedRender` from a previously-saved tracked string. Enables cache-backed reverse-diff: callers can persist `tracked()` output, then later feed it back into `generate_diff` without re-rendering. Useful for tools (like dodot's clean filter) that need to compute reverse-diffs on every git read but can't afford to re-evaluate templates that touch secret providers or expensive contexts.

## [0.2.0] - 2026-04-28

### Changed

- **Releases now run end-to-end in CI via `scripts/release`.** Triggering a release with `scripts/release <version|major|minor|patch>` queues a `workflow_dispatch` run that performs the version bump, `## [Unreleased]` roll, commit, tag, GitHub Release, multi-platform build (mac arm64 signed+notarized, linux x86_64+arm64), `.deb` attach, crates.io publish (burgertocow-lib then burgertocow), and Homebrew formula push to `arthur-debert/homebrew-tools` — all in CI. Replaces the previous tag-push trigger model.
- **macOS arm64 binaries are now Developer ID signed and Apple-notarized** so `brew install` works without Gatekeeper warnings.

### Added

- **Homebrew installation via `arthur-debert/homebrew-tools` tap.** Install with `brew install arthur-debert/tools/burgertocow`.
- **`.deb` packages for Debian/Ubuntu (amd64 + arm64).** Built by `cargo deb` in CI using the new `[package.metadata.deb]` block in `crates/burgertocow-cli/Cargo.toml` and attached to each GitHub Release.

## [0.1.0] - 2026-04-18

### Added

- Initial release.
- `burgertocow-lib` (`burgertocow` crate name): `Tracker` type wrapping
  `minijinja::Environment` with a tracking formatter that marks variable
  emissions with `U+001E`/`U+001F` boundaries.
- `TrackedRender` carrying both clean output and the tracked output.
- `generate_diff` function that reverse-maps modifications on a render back
  to a unified diff against the source template, using skeleton extraction
  + LCS alignment (via the `similar` crate).
- Conflict markers (`<<<< diff decision needed >>>>`) emitted for changes
  that cannot be safely aligned (e.g. edits to non-first loop iterations).
- `burgertocow` CLI with `render` and `diff` subcommands.
