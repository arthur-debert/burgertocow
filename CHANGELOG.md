# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
