use crate::parser::*;
use crate::engine::{VAR_START, VAR_END};
use similar::{TextDiff, DiffOp};

pub const CONFLICT_START: &str = "<<<< diff decision needed: start >>>>\n# the original template contained:\n";
pub const CONFLICT_MID: &str = "# and the updated version has this, resolve this manually\n";
pub const CONFLICT_END: &str = "<<<< diff decision needed: end >>>>\n";

fn map_skeletons(r_skel: &[char], t_skel: &[char]) -> Vec<Option<usize>> {
    let diff = TextDiff::from_chars(r_skel, t_skel);
    let mut map = vec![None; r_skel.len()];
    for op in diff.ops() {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for i in 0..*len {
                map[*old_index + i] = Some(*new_index + i);
            }
        }
    }
    map
}

fn is_purely_variable(pure: &PureRenderMap, old_start: usize, old_end: usize) -> bool {
    if old_start == old_end {
        if old_start == 0 || old_start == pure.text.len() {
            return false;
        }
        pure.is_variable[old_start.saturating_sub(1)] && pure.is_variable[old_start]
    } else {
        (old_start..old_end).all(|i| pure.is_variable[i])
    }
}

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
        Err(idx) => if idx > 0 { idx - 1 } else { 0 },
    };
    
    let ts_idx = tr_to_ts[rs_idx]?;
    Some(t_skel.char_mapping[ts_idx])
}

// Expands char index to line start/end
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

pub fn generate_diff(template_src: &str, tracked_render: &str, modified_src: &str) -> String {
    let t_chars: Vec<char> = template_src.chars().collect();
    let mod_chars: Vec<char> = modified_src.chars().collect();
    
    let t_skel = extract_template_skeleton(template_src);
    let v_start = VAR_START.chars().next().unwrap();
    let v_end = VAR_END.chars().next().unwrap();
    let r_skel = extract_render_skeleton(tracked_render, v_start, v_end);
    
    let tr_to_ts = map_skeletons(&r_skel.skeleton, &t_skel.skeleton);
    let pure = extract_pure_render(tracked_render, v_start, v_end);
    
    let diff = TextDiff::from_chars(&pure.text, &mod_chars);
    
    // We will build Replacements: (t_start, t_end, new_chars)
    // Applying backward ensures indices remain valid.
    let mut replacements = Vec::new();
    let mut ambiguous = None;

    for op in diff.ops() {
        if matches!(op, DiffOp::Equal { .. }) { continue; }
        
        let (old_start, old_end, new_start, new_end) = match op {
            DiffOp::Insert { old_index, new_index, new_len } => (*old_index, *old_index, *new_index, *new_index + *new_len),
            DiffOp::Delete { old_index, old_len, new_index } => (*old_index, *old_index + *old_len, *new_index, *new_index),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => (*old_index, *old_index + *old_len, *new_index, *new_index + *new_len),
            _ => continue,
        };
        
        if is_purely_variable(&pure, old_start, old_end) {
            continue; // Variable change only
        }
        
        let t_start_opt = map_pure_to_template(old_start, &pure, &r_skel, &tr_to_ts, &t_skel, t_chars.len());
        let t_end_opt = map_pure_to_template(old_end, &pure, &r_skel, &tr_to_ts, &t_skel, t_chars.len());
        
        match (t_start_opt, t_end_opt) {
            (Some(s), Some(e)) if s <= e => {
                replacements.push((s, e, mod_chars[new_start..new_end].to_vec()));
            }
            _ => {
                // Ambiguous mapping (e.g. they edited an unaligned loop iteration)
                ambiguous = Some((old_start, old_end, new_start, new_end));
                break;
            }
        }
    }
    
    if let Some((old_start, _old_end, new_start, new_end)) = ambiguous {
        let mut final_out = String::new();
        final_out.push_str(CONFLICT_START);
        
        // Find closest valid template indices for context
        let mut s_idx = old_start;
        let mut t_closest = None;
        while t_closest.is_none() {
            t_closest = map_pure_to_template(s_idx, &pure, &r_skel, &tr_to_ts, &t_skel, t_chars.len());
            if s_idx == 0 { break; }
            s_idx -= 1;
        }
        let (t_line_s, t_line_e) = get_line_range(&t_chars, t_closest.unwrap_or(0), t_closest.unwrap_or(t_chars.len()));
        
        let original_template_lines: String = t_chars[t_line_s..t_line_e].iter().collect();
        final_out.push_str(&original_template_lines);
        if !original_template_lines.ends_with('\n') { final_out.push('\n'); }
        
        final_out.push_str(CONFLICT_MID);
        let updated_text: String = mod_chars[new_start..new_end].iter().collect();
        final_out.push_str(&updated_text);
        if !updated_text.ends_with('\n') { final_out.push('\n'); }
        
        final_out.push_str(CONFLICT_END);
        return final_out;
    }
    
    // Check for overlaps? Reversing works if they are distinct
    replacements.sort_by_key(|(s, _, _)| *s);
    replacements.reverse();
    
    let mut modified_template_chars = t_chars.clone();
    for (s, e, new_chars) in replacements {
        let _ = modified_template_chars.splice(s..e, new_chars);
    }
    
    let modified_template_str: String = modified_template_chars.into_iter().collect();
    
    // Produce standard diff
    TextDiff::from_lines(template_src, &modified_template_str)
        .unified_diff()
        .header("template", "modified")
        .to_string()
}
