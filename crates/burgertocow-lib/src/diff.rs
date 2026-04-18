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

pub const CONFLICT_START: &str =
    "<<<< diff decision needed: start >>>>\n# the original template contained:\n";
pub const CONFLICT_MID: &str = "# and the updated version has this, resolve this manually\n";
pub const CONFLICT_END: &str = "<<<< diff decision needed: end >>>>\n";

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

/// Compute a unified diff expressed against the source template.
///
/// The returned string is either a unified diff (same format as
/// `git diff`) or a conflict block starting with [`CONFLICT_START`]. The
/// empty string is returned when the modification was a pure data change.
pub fn generate_diff(template_src: &str, tracked: &TrackedRender, modified_src: &str) -> String {
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
            return format_conflict(&t_chars, *closest_template_idx, new_chars);
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

fn format_conflict(t_chars: &[char], anchor: usize, new_chars: &[char]) -> String {
    let mut out = String::new();
    out.push_str(CONFLICT_START);
    let (ls, le) = get_line_range(t_chars, anchor, anchor);
    let orig: String = t_chars[ls..le].iter().collect();
    out.push_str(&orig);
    if !orig.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(CONFLICT_MID);
    let updated: String = new_chars.iter().collect();
    out.push_str(&updated);
    if !updated.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(CONFLICT_END);
    out
}

#[cfg(test)]
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
}
