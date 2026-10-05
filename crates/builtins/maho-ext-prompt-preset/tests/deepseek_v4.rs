use maho_ext_prompt_preset::deepseek_v4::DEEPSEEK_V4_RULES;

#[test]
fn preset_rules_route_distinct_reasoning_policies() {
    for preset in ["deepseek-v4-flash", "deepseek-v4-flash-0731", "deepseek-v4-pro"] {
        let ids: Vec<_> = DEEPSEEK_V4_RULES.iter().filter(|rule| rule.presets.contains(&preset)).map(|rule| rule.id).collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"injected-directive-authority"));
        assert_eq!(ids.contains(&"settled-reading"), preset != "deepseek-v4-pro");
        assert_eq!(ids.contains(&"reasoning-aim"), preset == "deepseek-v4-pro");
    }
}
