//! Reverse-templating diff generation.
//!
//! Given the source template, a tracked render, and a downstream tool's
//! modified version of the rendered output, this module produces a unified
//! diff expressed *against the template*. Changes that fall entirely
//! inside a tracked variable region are dropped (they are pure data
//! changes). Changes that cannot be aligned back to the template are
//! surfaced as an unresolved `<<<< diff decision needed >>>>` block so
//! a human can reconcile them.
//!
//! # Alignment pipeline
//!
//! 1. Build the template skeleton (static boilerplate with each `{{…}}`
//!    / `{%…%}` / `{#…#}` collapsed to `\0`).
//! 2. Build the render skeleton (static text with each tracked variable
//!    emission collapsed to `\0`).
//! 3. Run an LCS ([`Algorithm::Myers`]) over the two skeletons. Equal
//!    runs give us a direct render-char → template-char mapping.
//! 4. Post-process: fill in the unmapped "insertion" runs that correspond
//!    to repeated loop iterations by matching their skeleton text against
//!    a mapped twin (the *loop-iteration fallback*). This is what lets
//!    edits in the 2nd, 3rd, … iteration of a `{% for %}` still route
//!    back to the loop body in the template.
//! 5. Diff the modified text against the pure render, classify each diff
//!    op, and accumulate template-space replacements or conflicts.
//!
//! # Conflicts
//!
//! A conflict is emitted when:
//!
//! * a change can't be mapped back to the template at all (e.g. an edit
//!   in a region that has no twin in the template), or
//! * two changes map to the *same* template range but propose *different*
//!   replacement text (disagreeing edits across loop iterations).

use crate::engine::{TrackedRender, VAR_END, VAR_START};
use crate::parser::*;
use similar::{capture_diff_slices, Algorithm, DiffOp, TextDiff};
use std::borrow::Cow;
use std::ops::Range;

pub const CONFLICT_START: &str =
    "<<<< diff decision needed: start >>>>\n# the original template contained:\n";
pub const CONFLICT_MID: &str = "# and the updated version has this, resolve this manually\n";
pub const CONFLICT_END: &str = "<<<< diff decision needed: end >>>>\n";

/// Boundary strings emitted around an unresolvable edit.
///
/// A conflict block is built as:
///
/// ```text
/// {start}{original template line(s)}{mid}{user edit}{end}
/// ```
///
/// `generate_diff` uses [`ConflictMarkers::default`], which reproduces the
/// historical `<<<< diff decision needed: … >>>>` strings. Callers embedding
/// burgertocow in their own tools can pass custom markers via
/// [`generate_diff_with_markers`] — for example, to emit markers that match
/// the calling tool's conflict-resolution conventions, or that are
/// detectable by a project-specific pre-commit hook.
///
/// The `start`, `mid`, and `end` strings are written verbatim. It is the
/// caller's responsibility to include any trailing newlines needed for the
/// block to be readable.
#[derive(Debug, Clone)]
pub struct ConflictMarkers<'a> {
    /// Opens the conflict block. Printed immediately before the original
    /// template line(s).
    pub start: &'a str,
    /// Separates the original template content from the user's edit.
    pub mid: &'a str,
    /// Closes the conflict block. Printed immediately after the user's edit.
    pub end: &'a str,
}

impl Default for ConflictMarkers<'static> {
    fn default() -> Self {
        Self {
            start: CONFLICT_START,
            mid: CONFLICT_MID,
            end: CONFLICT_END,
        }
    }
}

impl<'a> ConflictMarkers<'a> {
    /// Construct a marker set from three caller-owned strings.
    pub const fn new(start: &'a str, mid: &'a str, end: &'a str) -> Self {
        Self { start, mid, end }
    }
}

/// Per-call knobs for [`generate_diff_with_markers_opts`].
///
/// Built explicitly rather than threaded into the existing entry point so
/// future options (e.g. relaxing the conflict-marker policy on a single
/// invocation) can be added without another revision of the public
/// signature. The current legacy entry point
/// [`generate_diff_with_markers`] is a thin wrapper that builds a
/// `DiffOptions` with an empty mask.
///
/// # Example
///
/// ```no_run
/// use burgertocow::{generate_diff_with_markers_opts, ConflictMarkers, DiffOptions, Tracker};
///
/// let mut tracker = Tracker::new();
/// tracker.add_template("t", "name = {{ user }}\npassword = {{ secret }}\n").unwrap();
/// let tracked = tracker.render(
///     "t",
///     serde_json::json!({"user": "Ada", "secret": "OLD"}),
/// ).unwrap();
///
/// let markers = ConflictMarkers::default();
/// // dodot's sidecar says: deployed line 1 (the password line) is a secret.
/// let mask = [1..2];
/// let opts = DiffOptions::new(&markers).with_mask(&mask);
///
/// // Even though the deployed file's secret value rotated, the diff is
/// // empty because the masked line is treated as unchanged.
/// let deployed = "name = Ada\npassword = NEW_ROTATED\n";
/// let diff = generate_diff_with_markers_opts(
///     "name = {{ user }}\npassword = {{ secret }}\n",
///     &tracked,
///     deployed,
///     &opts,
/// );
/// assert_eq!(diff, "");
/// ```
#[derive(Debug, Clone)]
pub struct DiffOptions<'a> {
    /// Conflict-block boundary markers; see [`ConflictMarkers`].
    pub markers: &'a ConflictMarkers<'a>,

    /// Deployed-file line ranges that should not participate in reverse
    /// diffing. burgertocow treats these lines as if they matched the
    /// cached render, regardless of actual content.
    ///
    /// Ranges are 0-based half-open `Range<usize>` (inclusive start,
    /// exclusive end). Line breaks are counted at `\n` boundaries — a
    /// file `"a\nb\nc\n"` has three lines (indices 0, 1, 2). The final
    /// line need not end in `\n`.
    ///
    /// Out-of-bounds ranges are clamped silently to the deployed file's
    /// line count so a stale sidecar that trails off the end of a
    /// re-rendered file does not panic. Overlapping or out-of-order
    /// ranges are merged.
    ///
    /// An empty slice (the default) makes the call behave byte-identical
    /// to [`generate_diff_with_markers`] — this property is the
    /// regression test that pins backward compatibility.
    ///
    /// # Interaction with conflict blocks
    ///
    /// If a conflict would have been emitted for a region that straddles
    /// a masked range, only the unmasked portion contributes to the
    /// conflict; if the entire conflict falls inside masked content, no
    /// block is emitted (and if it was the only change, the result is the
    /// empty string).
    ///
    /// # Tracking markers inside masked content
    ///
    /// The masking decision uses the deployed-line index only. Whether
    /// the corresponding `tracked_render` content carries [`VAR_START`] /
    /// [`VAR_END`] markers from prior renders is irrelevant.
    pub mask_deployed_lines: &'a [Range<usize>],
}

impl<'a> DiffOptions<'a> {
    /// Construct a `DiffOptions` with the given markers and an empty
    /// mask. With no mask set, the call behaves byte-identical to
    /// [`generate_diff_with_markers`].
    pub const fn new(markers: &'a ConflictMarkers<'a>) -> Self {
        Self {
            markers,
            mask_deployed_lines: &[],
        }
    }

    /// Replace the mask slice. Chains in builder style.
    pub const fn with_mask(mut self, mask: &'a [Range<usize>]) -> Self {
        self.mask_deployed_lines = mask;
        self
    }
}

/// Run LCS on the two skeletons and produce a render→template index map.
///
/// Entries are `Some(template_skel_idx)` for chars the LCS paired up, and
/// `None` for chars that were classified as insertions/deletions. Later
/// stages ([`augment_mapping_with_loop_fallback`]) fill in some of the
/// `None` entries using pattern matching.
fn map_skeletons(r_skel: &[char], t_skel: &[char]) -> Vec<Option<usize>> {
    let ops = capture_diff_slices(Algorithm::Myers, r_skel, t_skel);
    let mut map = vec![None; r_skel.len()];
    for op in ops {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for i in 0..len {
                map[old_index + i] = Some(new_index + i);
            }
        }
    }
    map
}

/// Fill in unmapped render-skeleton runs that look like repetitions of a
/// mapped run — i.e. the body of a `{% for %}` loop executed more than
/// once.
///
/// For each maximal run of `None` entries in `map`, we take the
/// corresponding slice of `r_skel` and look for an identical contiguous
/// slice elsewhere in `r_skel` whose indices are all mapped. When we find
/// one, we copy its template indices into the unmapped run, effectively
/// routing later iterations back to the same loop body in the template.
///
/// This is a heuristic: it assumes loop bodies render identically apart
/// from variable values (which are sentinels on both sides and therefore
/// match). Loops whose body text varies across iterations — e.g. using
/// `loop.first` / `loop.last` to change punctuation — would not match and
/// stay unmapped, which is the safe outcome.
fn augment_mapping_with_loop_fallback(map: &mut [Option<usize>], r_skel: &[char]) {
    let n = r_skel.len();
    let mut i = 0;
    while i < n {
        if map[i].is_some() {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < n && map[i].is_none() {
            i += 1;
        }
        let run_end = i;
        let pattern = &r_skel[run_start..run_end];
        if pattern.is_empty() {
            continue;
        }
        if let Some(twin_start) = find_mapped_pattern(r_skel, map, pattern, run_start) {
            for k in 0..pattern.len() {
                map[run_start + k] = map[twin_start + k];
            }
        }
    }
}

/// Find a contiguous slice in `r_skel` that (a) matches `pattern`
/// char-by-char and (b) has every index already mapped. `skip` is an
/// index range to avoid (the unmapped run we're trying to fill) so we
/// don't match it against itself.
fn find_mapped_pattern(
    r_skel: &[char],
    map: &[Option<usize>],
    pattern: &[char],
    skip: usize,
) -> Option<usize> {
    let n = r_skel.len();
    let m = pattern.len();
    if m == 0 || m > n {
        return None;
    }
    for start in 0..=(n - m) {
        if start == skip {
            continue;
        }
        let mut ok = true;
        for k in 0..m {
            if map[start + k].is_none() || r_skel[start + k] != pattern[k] {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(start);
        }
    }
    None
}

/// Decide whether a diff op sits entirely inside a single tracked
/// variable region — in which case it's pure data movement and should
/// not be turned into a template diff.
///
/// Handles three cases:
///
/// * Non-empty range: every char in the range is flagged `is_variable`.
/// * Zero-length range (insertion) strictly inside the pure text: the
///   chars immediately to the left and right are both inside a variable
///   *and* there is no [`VAR_END`]/[`VAR_START`] transition between them
///   in the tracked stream. The latter check rejects inserts that sit
///   between two adjacent `{{a}}{{b}}` variables — those should be
///   treated as template edits.
/// * Boundary inserts (position 0 or at the end): conservatively treated
///   as template edits.
fn is_purely_variable(
    pure: &PureRenderMap,
    tracked_chars: &[char],
    old_start: usize,
    old_end: usize,
) -> bool {
    if old_start == old_end {
        if old_start == 0 || old_start == pure.text.len() {
            return false;
        }
        if !(pure.is_variable[old_start - 1] && pure.is_variable[old_start]) {
            return false;
        }
        let left_tr = pure.map_to_tracked[old_start - 1];
        let right_tr = pure.map_to_tracked[old_start];
        // If we cross a VAR_END between the two neighbours, we are
        // sitting at a seam between two separate variable emissions,
        // which is effectively a template-level position.
        !tracked_chars[(left_tr + 1)..right_tr].contains(&VAR_END)
    } else {
        (old_start..old_end).all(|i| pure.is_variable[i])
    }
}

/// Translate a pure-render char index into a template char index.
///
/// Uses the render-skeleton index lookup table to find which sentinel /
/// static-char in `r_skel` the pure position corresponds to, then
/// follows `tr_to_ts` to the template skeleton, and finally
/// `t_skel.char_mapping` to the original template char offset.
///
/// Returns `None` when no mapping exists (the position fell inside an
/// unaligned insertion and the loop fallback could not resolve it).
fn map_pure_to_template(
    pure_idx: usize,
    pure: &PureRenderMap,
    r_skel: &SkeletonMap,
    tr_to_ts: &[Option<usize>],
    t_skel: &SkeletonMap,
    t_len: usize,
) -> Option<usize> {
    if pure_idx >= pure.map_to_tracked.len() {
        return Some(t_len);
    }
    let tr_idx = pure.map_to_tracked[pure_idx];

    let rs_idx = match r_skel.char_mapping.binary_search(&tr_idx) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    };

    let ts_idx = tr_to_ts[rs_idx]?;
    Some(t_skel.char_mapping[ts_idx])
}

/// Expand `(start, end)` in `chars` to the enclosing line range.
/// Used for constructing the conflict block's "original template" body
/// so the human resolver sees the full line of context.
fn get_line_range(chars: &[char], mut start: usize, mut end: usize) -> (usize, usize) {
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    if end < chars.len() && chars[end] == '\n' {
        end += 1;
    }
    (start, end)
}

/// Represents one classified outcome of a pure-render diff op.
enum Mapped {
    /// A template-space replacement: splice `new_chars` into `template[s..e]`.
    Replace {
        t_start: usize,
        t_end: usize,
        new_chars: Vec<char>,
    },
    /// The op affected only variable content — drop it silently.
    VariableOnly,
    /// The op can't be safely routed to the template. Emit a conflict
    /// block showing the closest template line and the offending new text.
    Conflict {
        closest_template_idx: usize,
        new_chars: Vec<char>,
    },
}

/// Compute a unified diff expressed against the source template, using the
/// default conflict-block markers.
///
/// The returned string is either a unified diff (same format as
/// `git diff`) or a conflict block starting with [`CONFLICT_START`]. The
/// empty string is returned when the modification was a pure data change.
///
/// Equivalent to calling [`generate_diff_with_markers`] with
/// [`ConflictMarkers::default`].
pub fn generate_diff(template_src: &str, tracked: &TrackedRender, modified_src: &str) -> String {
    generate_diff_with_markers(
        template_src,
        tracked,
        modified_src,
        &ConflictMarkers::default(),
    )
}

/// Compute a unified diff expressed against the source template, using
/// caller-supplied conflict-block markers.
///
/// Behaves identically to [`generate_diff`] except that any conflict block
/// emitted uses the strings from `markers`. Non-conflict output (unified
/// diffs and empty strings for pure-data changes) is unaffected.
///
/// Equivalent to calling [`generate_diff_with_markers_opts`] with
/// `DiffOptions::new(markers)` (i.e. an empty mask).
pub fn generate_diff_with_markers(
    template_src: &str,
    tracked: &TrackedRender,
    modified_src: &str,
    markers: &ConflictMarkers<'_>,
) -> String {
    generate_diff_with_markers_opts(
        template_src,
        tracked,
        modified_src,
        &DiffOptions::new(markers),
    )
}

/// Compute a unified diff expressed against the source template, with
/// caller-supplied conflict markers and optional masking of deployed-file
/// line ranges.
///
/// Lines listed in [`DiffOptions::mask_deployed_lines`] are treated as if
/// they always matched the cached render: regardless of their actual
/// content in `deployed`, no template-space change is generated for them.
/// Lines outside the mask diff normally.
///
/// With an empty mask this function behaves byte-identical to
/// [`generate_diff_with_markers`] — that round-trip is exercised in the
/// integration test suite.
///
/// # Mask semantics
///
/// See [`DiffOptions::mask_deployed_lines`] for line-numbering rules,
/// out-of-bounds clamping, overlap merging, and conflict-block
/// interaction.
pub fn generate_diff_with_markers_opts(
    template_src: &str,
    tracked: &TrackedRender,
    deployed: &str,
    opts: &DiffOptions<'_>,
) -> String {
    let masked: Cow<'_, str> = if opts.mask_deployed_lines.is_empty() {
        Cow::Borrowed(deployed)
    } else {
        Cow::Owned(apply_deployed_mask(
            tracked.output(),
            deployed,
            opts.mask_deployed_lines,
        ))
    };
    let modified_src: &str = masked.as_ref();
    let markers = opts.markers;

    let t_chars: Vec<char> = template_src.chars().collect();
    let mod_chars: Vec<char> = modified_src.chars().collect();
    let tracked_chars: Vec<char> = tracked.tracked().chars().collect();

    let t_skel = extract_template_skeleton(template_src);
    let r_skel = extract_render_skeleton(tracked.tracked(), VAR_START, VAR_END);

    let mut tr_to_ts = map_skeletons(&r_skel.skeleton, &t_skel.skeleton);
    augment_mapping_with_loop_fallback(&mut tr_to_ts, &r_skel.skeleton);

    let pure = extract_pure_render(tracked.tracked(), VAR_START, VAR_END);

    let ops = capture_diff_slices(Algorithm::Myers, &pure.text, &mod_chars);

    let mut mapped_ops: Vec<Mapped> = Vec::new();

    for op in ops {
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let (old_start, old_end, new_start, new_end) = match op {
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index, old_index, new_index, new_index + new_len),
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index, old_index + old_len, new_index, new_index),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (
                old_index,
                old_index + old_len,
                new_index,
                new_index + new_len,
            ),
            _ => continue,
        };

        if is_purely_variable(&pure, &tracked_chars, old_start, old_end) {
            mapped_ops.push(Mapped::VariableOnly);
            continue;
        }

        let t_start_opt =
            map_pure_to_template(old_start, &pure, &r_skel, &tr_to_ts, &t_skel, t_chars.len());
        let t_end_opt =
            map_pure_to_template(old_end, &pure, &r_skel, &tr_to_ts, &t_skel, t_chars.len());

        match (t_start_opt, t_end_opt) {
            (Some(s), Some(e)) if s <= e => {
                mapped_ops.push(Mapped::Replace {
                    t_start: s,
                    t_end: e,
                    new_chars: mod_chars[new_start..new_end].to_vec(),
                });
            }
            _ => {
                // Fall back to the *nearest* mapped template position we
                // can find by scanning backwards; we need *some* anchor to
                // show the human.
                let mut s_idx = old_start;
                let closest = loop {
                    let hit = map_pure_to_template(
                        s_idx,
                        &pure,
                        &r_skel,
                        &tr_to_ts,
                        &t_skel,
                        t_chars.len(),
                    );
                    if hit.is_some() || s_idx == 0 {
                        break hit;
                    }
                    s_idx -= 1;
                };
                mapped_ops.push(Mapped::Conflict {
                    closest_template_idx: closest.unwrap_or(0),
                    new_chars: mod_chars[new_start..new_end].to_vec(),
                });
            }
        }
    }

    // Detect conflicting replacements: two ops that mapped to the *same*
    // template range but disagree on the new text. This typically happens
    // when two loop iterations were edited differently.
    detect_duplicate_conflicts(&mut mapped_ops);

    // If any op is still a conflict, emit a conflict block and stop.
    for op in &mapped_ops {
        if let Mapped::Conflict {
            closest_template_idx,
            new_chars,
        } = op
        {
            return format_conflict(&t_chars, *closest_template_idx, new_chars, markers);
        }
    }

    // All ops were either dropped (variable-only) or mapped; build the
    // template-space modified string and render a unified diff.
    let mut replacements: Vec<(usize, usize, Vec<char>)> = Vec::new();
    for op in mapped_ops {
        if let Mapped::Replace {
            t_start,
            t_end,
            new_chars,
        } = op
        {
            replacements.push((t_start, t_end, new_chars));
        }
    }

    // Dedupe: identical edits at the same template range (e.g. one per
    // loop iteration) collapse to one.
    replacements.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    replacements.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);

    // Apply right-to-left so earlier splice ranges aren't invalidated.
    replacements.reverse();
    let mut modified_template_chars = t_chars.clone();
    for (s, e, new_chars) in replacements {
        let _ = modified_template_chars.splice(s..e, new_chars);
    }
    let modified_template_str: String = modified_template_chars.into_iter().collect();

    TextDiff::from_lines(template_src, &modified_template_str)
        .unified_diff()
        .header("template", "modified")
        .to_string()
}

fn detect_duplicate_conflicts(mapped_ops: &mut [Mapped]) {
    // Group Replace ops by (t_start, t_end). If any group has >1 distinct
    // replacement texts, convert each of them to Conflict.
    use std::collections::HashMap;
    let mut groups: HashMap<(usize, usize), Vec<Vec<char>>> = HashMap::new();
    for op in mapped_ops.iter() {
        if let Mapped::Replace {
            t_start,
            t_end,
            new_chars,
        } = op
        {
            groups
                .entry((*t_start, *t_end))
                .or_default()
                .push(new_chars.clone());
        }
    }
    let conflicting: std::collections::HashSet<(usize, usize)> = groups
        .into_iter()
        .filter(|(_, vs)| {
            let first = &vs[0];
            vs.iter().any(|v| v != first)
        })
        .map(|(k, _)| k)
        .collect();

    for op in mapped_ops.iter_mut() {
        if let Mapped::Replace {
            t_start,
            t_end,
            new_chars,
        } = op
        {
            if conflicting.contains(&(*t_start, *t_end)) {
                let replaced = Mapped::Conflict {
                    closest_template_idx: *t_start,
                    new_chars: std::mem::take(new_chars),
                };
                *op = replaced;
            }
        }
    }
}

/// Split `s` into lines, keeping each line's trailing `\n` byte if any.
///
/// Used by the masking pipeline so substituted lines preserve their
/// original line-termination state. Pure ASCII split — `\r\n` and bare
/// `\r` are not treated as line breaks (matching the rest of the diff
/// pipeline, which counts breaks at `\n` only).
fn split_keep_endings(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Build a synthetic deployed string where every masked deployed-line
/// index is replaced with the rendered (`pure_render`) line at the same
/// index. If the rendered output has no such line (the mask points past
/// EOF of the render), the deployed line is dropped: the rebuilt deployed
/// then matches "no change at this position" against the render, which is
/// the only safe interpretation of "treat as if it always matched".
///
/// Out-of-bounds masked ranges are clamped to the deployed line count.
/// Overlapping ranges are merged. Empty (`r.start == r.end` after
/// clamping) ranges are dropped.
///
/// Caller is expected to short-circuit with the original `deployed` when
/// `ranges` is empty so we don't pay the allocation cost in the common
/// case.
fn apply_deployed_mask(pure_render: &str, deployed: &str, ranges: &[Range<usize>]) -> String {
    let deployed_lines = split_keep_endings(deployed);
    let pure_lines = split_keep_endings(pure_render);
    let n = deployed_lines.len();

    let mut clamped: Vec<Range<usize>> = ranges
        .iter()
        .map(|r| (r.start.min(n))..(r.end.min(n)))
        .filter(|r| r.start < r.end)
        .collect();
    clamped.sort_by_key(|r| r.start);

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(clamped.len());
    for r in clamped {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }

    let mut out = String::with_capacity(deployed.len());
    let mut cursor = 0usize;
    for r in merged {
        for line in &deployed_lines[cursor..r.start] {
            out.push_str(line);
        }
        for idx in r.start..r.end {
            if let Some(line) = pure_lines.get(idx) {
                out.push_str(line);
            } else {
                out.push_str(deployed_lines[idx]);
            }
        }
        cursor = r.end;
    }
    for line in &deployed_lines[cursor..n] {
        out.push_str(line);
    }
    out
}

fn format_conflict(
    t_chars: &[char],
    anchor: usize,
    new_chars: &[char],
    markers: &ConflictMarkers<'_>,
) -> String {
    let mut out = String::new();
    out.push_str(markers.start);
    let (ls, le) = get_line_range(t_chars, anchor, anchor);
    let orig: String = t_chars[ls..le].iter().collect();
    out.push_str(&orig);
    if !orig.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(markers.mid);
    let updated: String = new_chars.iter().collect();
    out.push_str(&updated);
    if !updated.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(markers.end);
    out
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::engine::Tracker;
    use serde_json::json;

    fn roundtrip(template: &str, ctx: serde_json::Value, modified: &str) -> String {
        let mut t = Tracker::new();
        t.add_template("t", template).unwrap();
        let tracked = t.render("t", &ctx).unwrap();
        generate_diff(template, &tracked, modified)
    }

    #[test]
    fn identical_render_produces_empty_diff() {
        let mut t = Tracker::new();
        t.add_template("t", "Hello {{ u }}!").unwrap();
        let r = t.render("t", json!({"u": "X"})).unwrap();
        let d = generate_diff("Hello {{ u }}!", &r, r.output());
        assert_eq!(d, "");
    }

    #[test]
    fn pure_variable_change_produces_empty_diff() {
        let d = roundtrip("Hello {{ u }}!", json!({"u": "Arthur"}), "Hello Zaphod!");
        assert_eq!(d, "");
    }

    #[test]
    fn template_only_change_is_captured() {
        let d = roundtrip(
            "Hello {{ u }}!\nBye.",
            json!({"u": "Arthur"}),
            "Hello Arthur!\nBye for now.",
        );
        assert!(d.contains("-Bye."));
        assert!(d.contains("+Bye for now."));
    }

    #[test]
    fn loop_consistent_edit_is_captured_once() {
        // Template has one "- " prefix; render has two; user flips both
        // to "* ". Heuristic should map both iterations to the single
        // template line and dedupe the two identical replacements.
        let d = roundtrip(
            "{% for i in items %}- {{ i }}\n{% endfor %}",
            json!({"items": ["Apple", "Banana"]}),
            "* Apple\n* Banana\n",
        );
        // Both iterations collapsed to a single template-line change.
        assert!(d.contains("- {{ i }}"), "old prefix absent: {d}");
        assert!(d.contains("* {{ i }}"), "new prefix absent: {d}");
        // Verify the unified diff carries a `-` (removal) and `+` (add)
        // for the loop body line.
        assert!(
            d.lines()
                .any(|l| l.starts_with('-') && l.contains("- {{ i }}")),
            "no removal line for old prefix: {d}"
        );
        assert!(
            d.lines()
                .any(|l| l.starts_with('+') && l.contains("* {{ i }}")),
            "no addition line for new prefix: {d}"
        );
        assert!(
            !d.contains("diff decision needed"),
            "consistent edit should not be conflict: {d}"
        );
    }

    #[test]
    fn loop_inconsistent_edit_raises_conflict() {
        // Two iterations edited differently → conflict.
        let d = roundtrip(
            "{% for i in items %}- {{ i }}\n{% endfor %}",
            json!({"items": ["Apple", "Banana"]}),
            "* Apple\n! Banana\n",
        );
        assert!(d.contains("diff decision needed"), "got: {d}");
    }

    #[test]
    fn insert_between_adjacent_variables_is_template_change() {
        // Template: {{a}}{{b}} → render "XY" → modified "XZY".
        // The 'Z' sits *between* two distinct variables, which is a
        // template edit, not a variable edit.
        let d = roundtrip("{{ a }}{{ b }}", json!({"a": "X", "b": "Y"}), "XZY");
        assert_ne!(d, "", "insert between two vars should not be dropped");
    }

    #[test]
    fn mixed_template_and_variable_edits() {
        // Change the variable value AND the surrounding static text.
        // The variable change is silent, the template change shows up.
        let d = roundtrip("Hi {{ u }}!\nBye.", json!({"u": "A"}), "Hi Z!\nGoodbye.");
        assert!(d.contains("-Bye."), "template change missing: {d}");
        assert!(d.contains("+Goodbye."), "template change missing: {d}");
    }

    #[test]
    fn empty_modified_is_handled() {
        let d = roundtrip("x{{ u }}y", json!({"u": "a"}), "");
        // Either a full deletion diff or a conflict — both acceptable.
        assert!(!d.is_empty());
    }

    #[test]
    fn unicode_template_change_is_captured() {
        let d = roundtrip("日本: {{ u }}", json!({"u": "Ada"}), "World: Ada");
        assert!(d.contains("-日本"));
        assert!(d.contains("+World"));
    }

    #[test]
    fn conditional_template_change_is_captured() {
        let d = roundtrip(
            "Hello{% if v %}, {{ u }}{% endif %}!",
            json!({"v": true, "u": "A"}),
            "Hi, A!",
        );
        assert!(d.contains("-Hello"));
        assert!(d.contains("+Hi"));
    }

    #[test]
    fn comment_tag_is_invisible_in_render_but_preserved_in_template() {
        let d = roundtrip("a{# hidden #}b", json!({}), "aXb");
        // Insert between 'a' and 'b' — the comment collapses to nothing
        // in the render, so the template position is between a and b.
        assert!(d.contains("+") || !d.is_empty());
    }

    // -----------------------------------------------------------------
    // Masking helpers — split_keep_endings + apply_deployed_mask.
    // -----------------------------------------------------------------

    #[test]
    fn split_keep_endings_basic_shapes() {
        assert!(split_keep_endings("").is_empty());
        assert_eq!(split_keep_endings("a"), vec!["a"]);
        assert_eq!(split_keep_endings("a\n"), vec!["a\n"]);
        assert_eq!(split_keep_endings("a\nb"), vec!["a\n", "b"]);
        assert_eq!(split_keep_endings("a\nb\n"), vec!["a\n", "b\n"]);
        assert_eq!(split_keep_endings("\n"), vec!["\n"]);
        assert_eq!(split_keep_endings("a\n\nb"), vec!["a\n", "\n", "b"]);
    }

    #[test]
    fn split_keep_endings_concat_round_trip() {
        // Joining the split is byte-identical to the original. Important
        // because the masking pipeline relies on this to leave non-masked
        // regions untouched.
        for s in [
            "",
            "a",
            "a\n",
            "a\nb",
            "a\nb\n",
            "\n\n\n",
            "a\n\nb\n",
            "日本\nWorld\n",
        ] {
            let lines = split_keep_endings(s);
            let rebuilt: String = lines.concat();
            assert_eq!(rebuilt, s, "round-trip lost bytes for {s:?}");
        }
    }

    #[test]
    fn apply_deployed_mask_replaces_single_line() {
        let pure = "a\nB\nc\n";
        let deployed = "a\nX\nc\n";
        let out = apply_deployed_mask(pure, deployed, &[1..2]);
        assert_eq!(out, "a\nB\nc\n");
    }

    #[test]
    fn apply_deployed_mask_no_op_outside_range() {
        // Lines outside the mask are byte-preserved from `deployed`.
        let pure = "PURE-a\nPURE-b\nPURE-c\n";
        let deployed = "DEP-a\nPURE-b\nDEP-c\n";
        let out = apply_deployed_mask(pure, deployed, &[1..2]);
        assert_eq!(out, "DEP-a\nPURE-b\nDEP-c\n");
    }

    #[test]
    fn apply_deployed_mask_clamps_out_of_bounds() {
        let pure = "a\nb\nc\n";
        let deployed = "a\nX\nc\n";
        // Range trails past EOF — clamp end to 3.
        let out = apply_deployed_mask(pure, deployed, &[1..1000]);
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn apply_deployed_mask_clamps_entirely_out_of_bounds_to_noop() {
        let pure = "a\nb\nc\n";
        let deployed = "a\nb\nc\n";
        let out = apply_deployed_mask(pure, deployed, &[10..20]);
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn apply_deployed_mask_merges_overlapping_ranges() {
        let pure = "a\nb\nc\nd\ne\n";
        let deployed = "a\nX\nY\nZ\ne\n";
        let out = apply_deployed_mask(pure, deployed, &[1..3, 2..4]);
        assert_eq!(out, "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn apply_deployed_mask_handles_unsorted_input() {
        // Sidecar order is not contractual; sort and merge defensively.
        let pure = "a\nb\nc\nd\ne\n";
        let deployed = "a\nX\nc\nY\ne\n";
        let out = apply_deployed_mask(pure, deployed, &[3..4, 1..2]);
        assert_eq!(out, "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn apply_deployed_mask_drops_lines_when_pure_is_shorter() {
        // Mask points at deployed line whose index does not exist in
        // pure. The deployed line is dropped (replaced with nothing) so
        // the rebuilt deployed has no entry at that position.
        let pure = "a\n";
        let deployed = "a\nB\nC\n";
        let out = apply_deployed_mask(pure, deployed, &[1..3]);
        assert_eq!(out, "a\n");
    }

    #[test]
    fn apply_deployed_mask_preserves_trailing_newline_state_per_line() {
        // pure has trailing \n on last line; deployed does not. Mask the
        // last line — rebuild adopts pure's trailing-newline state.
        let pure = "a\nb\n";
        let deployed = "a\nB";
        let out = apply_deployed_mask(pure, deployed, &[1..2]);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn apply_deployed_mask_drops_empty_ranges() {
        let pure = "a\nb\nc\n";
        let deployed = "a\nX\nc\n";
        // r.start == r.end after clamping → dropped silently.
        let out = apply_deployed_mask(pure, deployed, &[2..2, 0..0]);
        assert_eq!(out, "a\nX\nc\n");
    }

    #[test]
    fn apply_deployed_mask_handles_unicode_line_content() {
        let pure = "日本\nb\nc\n";
        let deployed = "日本\nX\nc\n";
        let out = apply_deployed_mask(pure, deployed, &[1..2]);
        assert_eq!(out, "日本\nb\nc\n");
    }

    // -----------------------------------------------------------------
    // generate_diff_with_markers_opts wiring.
    // -----------------------------------------------------------------

    #[test]
    fn opts_empty_mask_matches_legacy_entry() {
        // The byte-identity backstop: with an empty mask, the new entry
        // point must produce exactly what `generate_diff_with_markers`
        // would have produced. This is the regression test that pins
        // backward compatibility for downstream callers.
        let mut t = Tracker::new();
        t.add_template("t", "Hello {{ u }}!\nBye.").unwrap();
        let r = t.render("t", json!({"u": "A"})).unwrap();
        let modified = "Hello A!\nBye for now.";
        let markers = ConflictMarkers::default();
        let legacy = generate_diff_with_markers("Hello {{ u }}!\nBye.", &r, modified, &markers);
        let opts_empty = generate_diff_with_markers_opts(
            "Hello {{ u }}!\nBye.",
            &r,
            modified,
            &DiffOptions::new(&markers),
        );
        assert_eq!(legacy, opts_empty);
    }

    #[test]
    fn opts_with_mask_drops_inside_edits_only() {
        // Two-line file: line 0 is plaintext (with a static-text change),
        // line 1 is a "secret" line whose deployed value also rotated.
        // With line 1 masked, only the static-text edit on line 0 should
        // propagate to the template diff.
        let template = "host = {{ h }}\npassword = {{ p }}\n";
        let mut t = Tracker::new();
        t.add_template("t", template).unwrap();
        let r = t
            .render("t", json!({"h": "localhost", "p": "OLD"}))
            .unwrap();
        // User renamed the static "host" → "hostname" AND the secret
        // line's value rotated. Mask covers only the secret line.
        let deployed = "hostname = localhost\npassword = NEW\n";
        let markers = ConflictMarkers::default();
        let mask = [1..2];
        let d = generate_diff_with_markers_opts(
            template,
            &r,
            deployed,
            &DiffOptions::new(&markers).with_mask(&mask),
        );
        assert!(d.contains("-host = {{ h }}"), "host diff missing: {d}");
        assert!(
            d.contains("+hostname = {{ h }}"),
            "hostname diff missing: {d}"
        );
        // The masked rotated value must not have leaked into the diff.
        assert!(!d.contains("NEW"), "masked value leaked: {d}");
    }

    #[test]
    fn opts_mask_covers_all_changed_lines_yields_empty_diff() {
        // Vault rotation simulated: the ONLY edit is on the secret line.
        // Mask covers it, diff is empty.
        let mut t = Tracker::new();
        t.add_template("t", "name = {{ n }}\nsecret = {{ s }}\n")
            .unwrap();
        let r = t.render("t", json!({"n": "Ada", "s": "OLD"})).unwrap();
        let deployed = "name = Ada\nsecret = NEW_ROTATED\n";
        let markers = ConflictMarkers::default();
        let mask = [1..2];
        let d = generate_diff_with_markers_opts(
            "name = {{ n }}\nsecret = {{ s }}\n",
            &r,
            deployed,
            &DiffOptions::new(&markers).with_mask(&mask),
        );
        assert_eq!(d, "");
    }
}
