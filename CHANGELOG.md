# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
