use burgertocow_lib::engine::Tracker;
use burgertocow_lib::diff::generate_diff;
use std::fs;

fn run_diff(template_file: &str, data_file: &str, modified_file: &str) -> String {
    let t_str = fs::read_to_string(template_file).unwrap();
    let d_str = fs::read_to_string(data_file).unwrap();
    let m_str = fs::read_to_string(modified_file).unwrap();
    let ctx: serde_json::Value = serde_json::from_str(&d_str).unwrap();
    
    let mut tracker = Tracker::new();
    let (_, tracked) = tracker.render("test", &t_str, &ctx).unwrap();
    
    generate_diff(&t_str, &tracked, &m_str)
}

#[test]
fn test_template_text_modification() {
    let diff = run_diff(
        "../../tests/fixtures/simple.md", 
        "../../tests/fixtures/simple_data.json", 
        "../../tests/fixtures/simple_mod_text.md"
    );
    assert!(diff.contains("-Welcome to the template."));
    assert!(diff.contains("+Welcome to the modified template."));
}

#[test]
fn test_pure_variable_modification() {
    let diff = run_diff(
        "../../tests/fixtures/simple.md", 
        "../../tests/fixtures/simple_data.json", 
        "../../tests/fixtures/simple_mod_var.md"
    );
    assert_eq!(diff.trim(), "");
}

#[test]
fn test_ambiguous_loop_modification() {
    let diff = run_diff(
        "../../tests/fixtures/loop.md", 
        "../../tests/fixtures/loop_data.json", 
        "../../tests/fixtures/loop_mod.md"
    );
    assert!(diff.contains("<<<< diff decision needed: start >>>>"));
    assert!(diff.contains("resolve this manually"));
}
