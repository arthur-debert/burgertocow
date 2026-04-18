//! Skeleton extraction for both source templates and tracked renders.
//!
//! A "skeleton" is the static-text backbone of a string with all dynamic
//! parts (template tags or tracked variable emissions) replaced by a single
//! sentinel character `\0`. Skeletons from the template and from the render
//! can then be aligned character-by-character with an LCS to recover a
//! mapping between rendered positions and source-template positions.

#[derive(Debug, Clone)]
pub struct SkeletonMap {
    pub skeleton: Vec<char>,
    /// For each char in `skeleton`, its starting char index in the original text.
    pub char_mapping: Vec<usize>,
}

/// Mask every `{{ … }}`, `{% … %}`, `{# … #}` cluster in a minijinja template
/// with a single `\0`, preserving all surrounding static text.
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
        skeleton.push(chars[i]);
        char_mapping.push(i);
        i += 1;
    }

    SkeletonMap {
        skeleton,
        char_mapping,
    }
}

/// The tracked render without its markers, with per-char metadata recording
/// (a) the marker-inclusive index and (b) whether that character came from
/// a variable emission.
#[derive(Debug)]
pub struct PureRenderMap {
    pub text: Vec<char>,
    pub map_to_tracked: Vec<usize>,
    pub is_variable: Vec<bool>,
}

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
