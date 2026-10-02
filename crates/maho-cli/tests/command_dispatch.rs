use maho_cli::cli::experimental::command::*;
#[tokio::test]
async fn dispatches_subcommand_builder_and_action() {
    let mut child = Command::<String, std::sync::Arc<std::sync::Mutex<Vec<String>>>>::new("child");
    child.option(string_option("--value", false)).unwrap();
    child.build(|input| Ok(input.value("--value").unwrap_or("").to_owned()));
    child.action(|value, context| Box::pin(async move { context.lock().unwrap().push(value); Ok(()) }));
    let mut root = Command::new("root"); root.command(child).unwrap();
    let values = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    assert_eq!(root.execute(&["child".to_owned(), "--value=hello".to_owned()], values.clone()).await.unwrap().unwrap(), "hello");
    assert_eq!(*values.lock().unwrap(), ["hello"]);
}
#[test]
fn aggregates_parse_and_builder_errors_and_rejects_duplicate_registration() {
    let mut command = Command::<String, ()>::new("command");
    command.option(string_option("--value", false)).unwrap();
    assert!(command.option(string_option("--value", false)).is_err());
    command.build(|_| Err(vec!["builder error".to_owned()]));
    let errors = command.parse(&["--value".to_owned()]).unwrap().unwrap_err();
    assert_eq!(errors.len(), 2);
}
