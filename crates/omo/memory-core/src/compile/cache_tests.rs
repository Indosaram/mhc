use pretty_assertions::assert_eq;

use crate::compile::cache::MemoryBlockCache;
use crate::compile::compile::CompileMemoryBlockOptions;
use crate::compile::compile_test_support::{memory, repo_with};

#[test]
fn given_the_same_template_identity_and_head_when_compiled_twice_then_the_stable_projection_is_reused()
 {
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("DESC", "Stable Persona\n"),
    )])
    .expect("repo init");

    let cache = MemoryBlockCache::new();
    let options = CompileMemoryBlockOptions {
        agent_id: "agent-cache-1".to_string(),
    };

    let first = cache
        .compile(&repo, "template-a", &options)
        .expect("compile 1");
    assert_eq!(cache.size(), 1);

    let second = cache
        .compile(&repo, "template-a", &options)
        .expect("compile 2");
    assert_eq!(first, second);
    assert_eq!(cache.size(), 1);
}

#[test]
fn given_either_template_content_or_head_changes_when_compiled_then_each_stable_key_retains_only_its_latest_revision()
 {
    let (_temp, repo) =
        repo_with(vec![("system/persona.md", &memory("DESC", "V1 Persona\n"))]).expect("repo init");

    let cache = MemoryBlockCache::new();
    let options = CompileMemoryBlockOptions {
        agent_id: "agent-cache-2".to_string(),
    };

    let v1 = cache
        .compile(&repo, "template-1", &options)
        .expect("v1 compile");
    assert!(v1.contains("V1 Persona"));
    assert_eq!(cache.size(), 1);

    let t2 = cache
        .compile(&repo, "template-2", &options)
        .expect("t2 compile");
    assert!(t2.contains("V1 Persona"));
    assert_eq!(cache.size(), 2);

    let persona_path = repo.dir.join("system/persona.md");
    std::fs::write(&persona_path, memory("DESC", "V2 Persona\n")).expect("write persona");
    repo.commit_write(
        &["system/persona.md"],
        "update persona",
        &crate::git::GitCommitAuthor {
            agent_id: "test-author".to_string(),
            author_name: "test-author".to_string(),
            author_email: None,
        },
    )
    .expect("commit write");

    let v2 = cache
        .compile(&repo, "template-1", &options)
        .expect("v2 compile");
    assert!(v2.contains("V2 Persona"));
    assert_eq!(cache.size(), 2);
}

#[test]
fn given_repeated_calls_for_one_stable_projection_when_compile_runs_many_times_then_the_cache_stays_bounded_to_one_entry()
 {
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("DESC", "Repeated Persona\n"),
    )])
    .expect("repo init");

    let cache = MemoryBlockCache::new();
    let options = CompileMemoryBlockOptions {
        agent_id: "agent-repeated".to_string(),
    };

    for _ in 0..10 {
        let compiled = cache
            .compile(&repo, "same-template", &options)
            .expect("compile");
        assert!(compiled.contains("Repeated Persona"));
        assert_eq!(cache.size(), 1);
    }
}

#[test]
fn given_two_identities_at_the_same_head_when_compiled_through_one_cache_then_identity_stable_projections_remain_isolated()
 {
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("DESC", "Shared Persona\n"),
    )])
    .expect("repo init");

    let cache = MemoryBlockCache::new();
    let opt_a = CompileMemoryBlockOptions {
        agent_id: "agent-aaa".to_string(),
    };
    let opt_b = CompileMemoryBlockOptions {
        agent_id: "agent-bbb".to_string(),
    };

    let res_a = cache.compile(&repo, "template", &opt_a).expect("compile a");
    let res_b = cache.compile(&repo, "template", &opt_b).expect("compile b");

    assert!(res_a.contains("- AGENT_ID: agent-aaa"));
    assert!(res_b.contains("- AGENT_ID: agent-bbb"));
    assert_eq!(cache.size(), 2);
}

#[test]
fn given_cache_clear_when_called_then_all_entries_are_purged() {
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("DESC", "Purge Persona\n"),
    )])
    .expect("repo init");

    let cache = MemoryBlockCache::new();
    let options = CompileMemoryBlockOptions {
        agent_id: "agent-purge".to_string(),
    };

    cache.compile(&repo, "template", &options).expect("compile");
    assert_eq!(cache.size(), 1);

    cache.clear();
    assert_eq!(cache.size(), 0);
}
