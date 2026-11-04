//! The completion limits survive `maho_config` loading (unknown task keys are stripped) and reach
//! `TaskSettings`.

use maho_config::{Env, LoadOptions, load};
use maho_task::TaskSettings;

#[test]
fn given_a_user_config_when_it_sets_the_completion_limits_then_task_settings_carry_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().canonicalize().expect("canonical").join("home");
    std::fs::create_dir_all(home.join(".omo")).expect("mkdir");
    std::fs::write(
        home.join(".omo/omo.jsonc"),
        r#"{"task": {"completion_transport_limit": 5000, "completion_batch_limit": 30000}}"#,
    )
    .expect("write");
    let mut env = Env::new();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());

    let loaded = load(
        &LoadOptions::new(&home)
            .with_env(env)
            .with_account_home(None)
            .with_catalog(true),
    );

    let settings = TaskSettings::from_view(Some(&loaded.view));
    assert_eq!(settings.completion_transport_limit, 5000);
    assert_eq!(settings.completion_batch_limit, 30000);
}
