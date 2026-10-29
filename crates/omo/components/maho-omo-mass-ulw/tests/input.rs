use maho_omo_mass_ulw::*;

#[test]
fn trigger_spellings() {
    for text in ["mass ulw", "massulw", "MASS ULW", "Mass-Ulw", "mass  ulw", "run mass ulw now", "mass-ulw", "mass ulw, then report back"] { assert!(is_mass_ulw_input(text), "{text}"); }
}
#[test]
fn near_misses() {
    for text in ["", "mass", "ulw", "ultrawork", "massive ulw", "xmassulw", "mass ulw2", "massachusetts", "mass ulw-loop is separate", "the mass of ulw"] { assert!(!is_mass_ulw_input(text), "{text}"); }
}
#[test] fn raw_skill_suppressed() { assert!(skill_already_invoked("/skill:mass-ulw run the graph")); }
#[test] fn expanded_skill_suppressed() { assert!(skill_already_invoked("<skill name=\"mass-ulw\" path=\"skills/mass-ulw/SKILL.md\">mass ulw</skill>")); }
#[test] fn expanded_skill_case_insensitive() { assert!(skill_already_invoked("<SKILL\tNAME=\"MASS-ULW\">mass ulw</skill>")); }
#[test] fn different_skill_not_suppressed() { assert!(!skill_already_invoked("/skill:other mass ulw")); }
#[test] fn command_requires_leading_prefix() { assert!(!skill_already_invoked("prefix /skill:mass-ulw mass ulw")); }
#[test] fn raw_skill_case_sensitive() { assert!(!skill_already_invoked("/skill:MASS-ULW mass ulw")); }
#[test] fn skill_name_boundary() { assert!(!skill_already_invoked("/skill:mass-ulw-other mass ulw")); }
#[test] fn root_is_used() { assert!(mass_ulw_skill_pointer("/package/skills/").contains("/package/skills/mass-ulw/SKILL.md")); }
