use maho_server::app_server::{registry::{MethodRegistry, RegistryConnection}, skills::register_skill_methods};
use serde_json::json;

#[tokio::test]
async fn skills_list_validates_params_and_reports_missing_directories() {
    let directory = tempfile::tempdir().unwrap();
    let mut registry = MethodRegistry::default();
    register_skill_methods(&mut registry, directory.path().join("agent").display().to_string(), directory.path().display().to_string());
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    for params in [json!(1), json!({"cwds":null}), json!({"cwds":[1]}), json!({"forceReload":null})] {
        let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"skills/list","params":params})).await;
        assert_eq!(response["error"]["code"], -32602);
    }
    let response = registry.dispatch(connection, json!({"id":2,"method":"skills/list","params":{"cwds":["absent"]}})).await;
    assert_eq!(response["result"]["data"][0]["errors"][0]["message"], "skill cwd does not exist");
}

#[tokio::test]
async fn skills_list_uses_native_loader_cache_and_explicit_reload() {
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let skill_dir = agent.join("skills/example");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: example\ndescription: Example skill\n---\nBody\n").unwrap();
    let mut registry = MethodRegistry::default();
    register_skill_methods(&mut registry, agent.display().to_string(), directory.path().display().to_string());
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    let request = json!({"id":1,"method":"skills/list"});
    let initial = registry.dispatch(connection.clone(), request.clone()).await;
    assert_eq!(initial["result"]["data"][0]["skills"][0]["scope"], "user");
    assert_eq!(initial["result"]["data"][0]["skills"][0]["name"], "example");
    std::fs::remove_file(skill_dir.join("SKILL.md")).unwrap();
    let cached = registry.dispatch(connection.clone(), request).await;
    assert_eq!(cached["result"], initial["result"]);
    let reloaded = registry.dispatch(connection, json!({"id":2,"method":"skills/list","params":{"forceReload":true}})).await;
    assert_eq!(reloaded["result"]["data"][0]["skills"], json!([]));
}
