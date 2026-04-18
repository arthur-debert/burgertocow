//! Skeleton extraction for both source templates and tracked renders.
//!
//! A *skeleton* is the static-text backbone of a string with all dynamic
//! parts (template tags or tracked variable emissions) replaced by a single
//! sentinel character (`\0`). Skeletons from the template and from the
//! render can then be aligned character-by-character with a longest-common-
//! subsequence pass to recover a mapping between rendered positions and
//! source-template positions.
//!
//! # Invariants
//!
//! * [`SkeletonMap::skeleton`] and [`SkeletonMap::char_mapping`] always
//!   have the same length; `char_mapping[i]` is a valid char index into
//!   the original input string.
//! * Each `{{ … }}`, `{% … %}`, `{# … #}` cluster in the source template
//!   collapses to exactly one `\0` — so a `for`-loop with N iterations in
//!   the render will emit N `\0`s, while the template side emits only one
//!   per tag. LCS consequently aligns the first rendered iteration and
//!   leaves the remaining ones as unmapped insertions; see the README for
//!   the trade-off discussion.
//!
//! # Limits of the "lightweight tokenizer"
//!
//! The tokenizer is intentionally simple and does *not* understand
//! minijinja's full lexer — in particular:
//!
//! * It does not recognise whitespace-control markers (`{%-`, `-%}`) as
//!   distinct — they are just part of the tag body.
//! * It does not handle `raw`/`endraw` blocks — a literal `{{` inside a
//!   `raw` block would still be treated as the start of a tag. This is
//!   fine for well-formed templates where tags outside `raw` mean what
//!   they look like.
//! * It matches the *first* closing sequence that pairs with the opening
//!   brace, which is what minijinja's own lexer does (minijinja does not
//!   support nested tags).

/// A string with every dynamic region collapsed to `\0`, plus a back-map
/// from skeleton indices to char indices in the original string.
#[derive(Debug, Clone)]
pub struct SkeletonMap {
    /// The skeleton text (static chars interleaved with `\0` sentinels).
    pub skeleton: Vec<char>,
    /// For each char in `skeleton`, its starting char index in the source.
    pub char_mapping: Vec<usize>,
}

/// Mask every `{{ … }}`, `{% … %}`, `{# … #}` cluster in a minijinja
/// template with a single `\0`, preserving all surrounding static text.
///
/// The function walks the source one char at a time. When it sees `{` it
/// peeks at the next char — if that forms a minijinja opening delimiter
/// it records one `\0`, remembers the position of the `{`, then advances
/// past the matching closing sequence. Unmatched opens (`{x` where `x` is
/// not in `{%#`) are treated as plain text.
pub fn extract_template_skeleton(template: &str) -> SkeletonMap {
    let mut skeleton = Vec::new();
    let mut char_mapping = Vec::new();

    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next == '{' || next == '%' || next == '#' {
                let close1 = match next {
                    '{' => '}',
                    '%' => '%',
                    _ => '#',
                };
                let close2 = '}';

                skeleton.push('\0');
                char_mapping.push(i);

                i += 2;
                while i < chars.len() {
                    if chars[i] == close1 && i + 1 < chars.len() && chars[i + 1] == close2 {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }

        skeleton.push(chars[i]);
        char_mapping.push(i);
        i += 1;
    }

    SkeletonMap {
        skeleton,
        char_mapping,
    }
}

/// Mask each tracked variable emission (text between `v_start`/`v_end`
/// markers) in a render with a single `\0`.
///
/// Unbalanced markers are tolerated: a stray `v_start` without a matching
/// `v_end` is swallowed until end-of-string, and a stray `v_end` is
/// ignored. This defensive behaviour prevents malformed input from
/// panicking the aligner, at the cost of a slightly degraded mapping.
pub fn extract_render_skeleton(tracked_render: &str, v_start: char, v_end: char) -> SkeletonMap {
    let mut skeleton = Vec::new();
    let mut char_mapping = Vec::new();

    let chars: Vec<char> = tracked_render.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == v_start {
            skeleton.push('\0');
            char_mapping.push(i);

            i += 1;
            while i < chars.len() {
                if chars[i] == v_end {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Defensive: a stray v_end with no open pair — skip it rather than
        // emit it as a static char, so it can't corrupt alignment.
        if chars[i] == v_end {
            i += 1;
            continue;
        }
        skeleton.push(chars[i]);
        char_mapping.push(i);
        i += 1;
    }

    SkeletonMap {
        skeleton,
        char_mapping,
    }
}

/// The tracked render without its markers, plus per-char metadata.
///
/// * [`text`](Self::text) — the visible chars in order (markers stripped).
/// * [`map_to_tracked`](Self::map_to_tracked) — the char index each pure
///   char occupied in the tracked (marker-carrying) string.
/// * [`is_variable`](Self::is_variable) — `true` iff the char was produced
///   by a variable emission (i.e. sits between a `v_start`/`v_end` pair).
#[derive(Debug)]
pub struct PureRenderMap {
    pub text: Vec<char>,
    pub map_to_tracked: Vec<usize>,
    pub is_variable: Vec<bool>,
}

/// Strip marker characters from a tracked render and return the pure
/// render along with variable-provenance metadata.
pub fn extract_pure_render(tracked_render: &str, v_start: char, v_end: char) -> PureRenderMap {
    let mut text = Vec::new();
    let mut map_to_tracked = Vec::new();
    let mut is_variable = Vec::new();

    let mut in_var = false;
    for (i, c) in tracked_render.chars().enumerate() {
        if c == v_start {
            in_var = true;
        } else if c == v_end {
            in_var = false;
        } else {
            text.push(c);
            map_to_tracked.push(i);
            is_variable.push(in_var);
        }
    }

    PureRenderMap {
        text,
        map_to_tracked,
        is_variable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: char = '\x1E';
    const VE: char = '\x1F';

    fn sk(s: &str) -> String {
        extract_template_skeleton(s).skeleton.into_iter().collect()
    }

    #[test]
    fn expression_tag_becomes_one_sentinel() {
        assert_eq!(sk("Hello {{ name }}!"), "Hello \0!");
    }

    #[test]
    fn statement_tag_becomes_one_sentinel() {
        assert_eq!(sk("a{% if x %}b{% endif %}c"), "a\0b\0c");
    }

    #[test]
    fn comment_tag_becomes_one_sentinel() {
        assert_eq!(sk("x{# ignore #}y"), "x\0y");
    }

    #[test]
    fn adjacent_tags_each_get_a_sentinel() {
        assert_eq!(sk("{{a}}{{b}}"), "\0\0");
    }

    #[test]
    fn literal_single_brace_is_preserved() {
        assert_eq!(sk("a { b } c"), "a { b } c");
    }

    #[test]
    fn whitespace_control_markers_are_absorbed_into_the_tag() {
        assert_eq!(sk("a{%- if x -%}b{%- endif -%}c"), "a\0b\0c");
    }

    #[test]
    fn char_mapping_points_to_opening_brace_for_tags() {
        let m = extract_template_skeleton("ab{{ v }}cd");
        // Char positions:  a=0 b=1 {=2 {=3 sp=4 v=5 sp=6 }=7 }=8 c=9 d=10
        // Skeleton chars:  a   b   \0                                c   d
        // Mapping points the sentinel at the opening '{' of the tag.
        assert_eq!(m.skeleton, vec!['a', 'b', '\0', 'c', 'd']);
        assert_eq!(m.char_mapping, vec![0, 1, 2, 9, 10]);
    }

    #[test]
    fn render_skeleton_collapses_each_variable() {
        let tracked = format!("Hi {VS}Ada{VE}, welcome");
        let m = extract_render_skeleton(&tracked, VS, VE);
        let skel: String = m.skeleton.iter().collect();
        assert_eq!(skel, "Hi \0, welcome");
    }

    #[test]
    fn render_skeleton_tolerates_unbalanced_end_marker() {
        // Defensive behaviour: a stray v_end is silently dropped.
        let tracked = format!("a{VE}b");
        let m = extract_render_skeleton(&tracked, VS, VE);
        let skel: String = m.skeleton.iter().collect();
        assert_eq!(skel, "ab");
    }

    #[test]
    fn pure_render_marks_variable_chars() {
        let tracked = format!("A{VS}Bob{VE}Z");
        let pure = extract_pure_render(&tracked, VS, VE);
        let txt: String = pure.text.iter().collect();
        assert_eq!(txt, "ABobZ");
        assert_eq!(pure.is_variable, vec![false, true, true, true, false]);
    }

    #[test]
    fn pure_render_handles_empty_variable() {
        let tracked = format!("[{VS}{VE}]");
        let pure = extract_pure_render(&tracked, VS, VE);
        let txt: String = pure.text.iter().collect();
        assert_eq!(txt, "[]");
        assert_eq!(pure.is_variable, vec![false, false]);
    }

    #[test]
    fn unicode_in_static_text_is_preserved() {
        assert_eq!(sk("日本 {{ x }} 语"), "日本 \0 语");
    }

    #[test]
    fn unclosed_tag_eats_to_end_of_template() {
        // Best-effort: an unclosed tag is collapsed to a single sentinel
        // and consumes the rest of the string, matching minijinja's own
        // parser behaviour (which would raise an error at render time).
        let m = extract_template_skeleton("hello {{ never_closed");
        let skel: String = m.skeleton.iter().collect();
        assert_eq!(skel, "hello \0");
    }
}
