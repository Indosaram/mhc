use std::path::Path;
use memory_core::{facts::{FactsKnownPerson,FactsPrimaryHuman},memfs::{MemoryFrontmatter,parse_memory_file}};
pub struct FactsPeoplePayload {pub known_people:Vec<FactsKnownPerson>,pub primary_human:FactsPrimaryHuman}
fn read_card(path:&Path)->Option<MemoryFrontmatter>{parse_memory_file(&std::fs::read_to_string(path).ok()?).ok().map(|parsed|parsed.frontmatter)}
pub fn read_facts_people_payload(repo:&Path)->FactsPeoplePayload {
    let human=read_card(&repo.join("system/human.md"));let people=repo.join("people");let mut known_people=Vec::new();
    if let Ok(entries)=std::fs::read_dir(&people){for entry in entries.flatten(){if !entry.file_type().is_ok_and(|kind|kind.is_dir())||entry.file_name()=="human"{continue;}let slug=entry.file_name().to_string_lossy().into_owned();let Some(card)=read_card(&entry.path().join("card.md"))else{continue;};let description=card.description;let mut display_name=description.as_str();if description.get(..6).is_some_and(|prefix|prefix.eq_ignore_ascii_case("Person")){let after=&description[6..];if let Some(after)=after.trim_start().strip_prefix('-'){display_name=after.trim_start();}}let display_name=display_name.trim();known_people.push(FactsKnownPerson{display_name:if display_name.is_empty(){slug.clone()}else{display_name.to_owned()},slug,aliases:card.aliases.unwrap_or_default()});}}
    known_people.sort_by(|left,right|left.slug.cmp(&right.slug));FactsPeoplePayload{known_people,primary_human:FactsPrimaryHuman{slug:"human".into(),aliases:human.and_then(|card|card.aliases).unwrap_or_default()}}
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn missing_people_and_human_are_empty(){let dir=tempfile::tempdir().unwrap();let payload=read_facts_people_payload(dir.path());assert!(payload.known_people.is_empty());assert_eq!(payload.primary_human,FactsPrimaryHuman{slug:"human".into(),aliases:vec![]});}
    #[test]fn valid_cards_sort_and_human_is_separate(){let dir=tempfile::tempdir().unwrap();for (path,content) in [("system/human.md","---\ndescription: Person - Human\naliases: [\"Indo\"]\n---\n"),("people/zeta/card.md","---\ndescription: Person - Zeta\naliases: [\"Z\"]\n---\n"),("people/alpha/card.md","---\ndescription: pErSoN - Alpha\n---\n"),("people/human/card.md","---\ndescription: duplicate\n---\n"),("people/bad/card.md","bad")]{let path=dir.path().join(path);std::fs::create_dir_all(path.parent().unwrap()).unwrap();std::fs::write(path,content).unwrap();}let payload=read_facts_people_payload(dir.path());assert_eq!(payload.known_people.iter().map(|person|person.slug.as_str()).collect::<Vec<_>>(),["alpha","zeta"]);assert_eq!(payload.known_people[0].display_name,"Alpha");assert_eq!(payload.known_people[1].aliases,["Z"]);assert_eq!(payload.primary_human.aliases,["Indo"]);}
}
