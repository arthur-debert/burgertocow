#[derive(Debug, Clone)]
pub struct SkeletonMap {
    pub skeleton: Vec<char>,
    // For each char in skeleton, its starting char index in the original text
    pub char_mapping: Vec<usize>,
}

/// A very crude minijinja tokenizer that masks all variables and tags
pub fn extract_template_skeleton(template: &str) -> SkeletonMap {
    let mut skeleton = Vec::new();
    let mut char_mapping = Vec::new();
    
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Tag openers: {{ {% {#
        if chars[i] == '{' && i + 1 < chars.len() {
            let next = chars[i+1];
            if next == '{' || next == '%' || next == '#' {
                // Determine closing tag: }} %} #}
                let close1 = if next == '{' { '}' } else if next == '%' { '%' } else { '#' };
                let close2 = '}';
                
                let start_idx = i;
                skeleton.push('\0');
                char_mapping.push(start_idx);
                
                i += 2; // skip opener
                // advance until closer
                while i < chars.len() {
                    if chars[i] == close1 && i + 1 < chars.len() && chars[i+1] == close2 {
                        i += 2; // skip closer
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
    
    SkeletonMap { skeleton, char_mapping }
}

/// Masks tracked variable outputs
pub fn extract_render_skeleton(tracked_render: &str, v_start: char, v_end: char) -> SkeletonMap {
    let mut skeleton = Vec::new();
    let mut char_mapping = Vec::new();
    
    let chars: Vec<char> = tracked_render.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == v_start {
            let start_idx = i;
            skeleton.push('\0');
            char_mapping.push(start_idx);
            
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
    
    SkeletonMap { skeleton, char_mapping }
}

/// Given TrackedRender, reconstruct the PureRender mapping each char 
/// to its TrackedRender index, and keeping track of variable boundaries!
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
    
    PureRenderMap { text, map_to_tracked, is_variable }
}
