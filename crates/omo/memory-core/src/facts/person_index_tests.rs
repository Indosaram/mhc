use std::fs::{create_dir_all, write};

use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::*;

#[test]
fn test_display_name_of_variants() {
    assert_eq!(display_name_of("Person - Jane Doe", "fallback"), "Jane Doe");
    assert_eq!(display_name_of("person - Bob", "fallback"), "Bob");
    assert_eq!(display_name_of("PERSON-Alice", "fallback"), "Alice");
    assert_eq!(
        display_name_of("The Architect", "fallback"),
        "The Architect"
    );
    assert_eq!(display_name_of("Person -   ", "fallback"), "fallback");
    assert_eq!(display_name_of("", "fallback"), "fallback");
}

#[test]
fn test_read_facts_people_index_empty_dir() {
    let tmp = tempdir().expect("tempdir");
    let index = read_facts_people_index(tmp.path());
    assert_eq!(index.is_empty(), true);
}

#[test]
fn test_read_facts_people_index_with_human_and_people() {
    let tmp = tempdir().expect("tempdir");
    let system_dir = tmp.path().join("system");
    create_dir_all(&system_dir).expect("create system dir");
    write(
        system_dir.join("human.md"),
        "---\ndescription: Person - Primary Human\naliases: [\"Boss\", \"Admin\"]\n---\nNotes\n",
    )
    .expect("write human.md");

    let alice_dir = tmp.path().join("people").join("alice");
    create_dir_all(&alice_dir).expect("create alice dir");
    write(
        alice_dir.join("card.md"),
        "---\ndescription: Person - Alice Smith\naliases: [\"Ali\"]\n---\nCard body\n",
    )
    .expect("write alice card.md");

    let bob_dir = tmp.path().join("people").join("bob");
    create_dir_all(&bob_dir).expect("create bob dir");
    write(
        bob_dir.join("card.md"),
        "---\ndescription: Bob Jones\n---\nCard body\n",
    )
    .expect("write bob card.md");

    let index = read_facts_people_index(tmp.path());
    assert_eq!(index.len(), 3);

    assert_eq!(index[0].slug, "human");
    assert_eq!(index[0].display_name, "Primary Human");
    assert_eq!(index[0].names, vec!["Primary Human", "Boss", "Admin"]);

    assert_eq!(index[1].slug, "alice");
    assert_eq!(index[1].display_name, "Alice Smith");
    assert_eq!(index[1].names, vec!["Alice Smith", "Ali"]);

    assert_eq!(index[2].slug, "bob");
    assert_eq!(index[2].display_name, "Bob Jones");
    assert_eq!(index[2].names, vec!["Bob Jones"]);
}
