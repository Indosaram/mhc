use std::collections::BTreeMap;
#[derive(Clone)]
pub struct CommandOption { pub name: String, pub flag: bool, pub repeatable: bool }
pub struct ParsedCommandInput { pub values: BTreeMap<String, Vec<String>>, pub remaining_args: Vec<String>, pub errors: Vec<String> }
impl ParsedCommandInput { pub fn value(&self, name: &str) -> Option<&str> { self.values.get(name).and_then(|v| v.first()).map(String::as_str) } }
pub fn parse_options(argv: &[String], options: &[CommandOption]) -> ParsedCommandInput {
    let mut parsed = ParsedCommandInput { values: BTreeMap::new(), remaining_args: Vec::new(), errors: Vec::new() }; let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index]; if argument == "--" { parsed.remaining_args.extend_from_slice(&argv[index..]); break; }
        let (name, inline) = argument.split_once('=').map_or((argument.as_str(), None), |(name, value)| (name, Some(value)));
        let Some(option) = options.iter().find(|option| option.name == name) else { parsed.remaining_args.extend_from_slice(&argv[index..]); break; };
        let value = if option.flag { if inline.is_some() { parsed.errors.push(format!("{name} does not take a value")); index += 1; continue; } "".to_owned() } else {
            let candidate = if let Some(value) = inline { Some(value) } else if let Some(value) = argv.get(index + 1).filter(|v| !v.starts_with('-')) { index += 1; Some(value.as_str()) } else { None };
            let Some(value) = candidate.filter(|value| !value.is_empty()) else { parsed.errors.push(format!("{name} requires a value")); index += 1; continue; }; value.to_owned()
        };
        let values = parsed.values.entry(name.to_owned()).or_default(); if !values.is_empty() && !option.repeatable { parsed.errors.push(format!("{name} may only be specified once")); } else { values.push(value); } index += 1;
    } parsed
}
pub fn string_option(name: &str, repeatable: bool) -> CommandOption { CommandOption { name: name.to_owned(), flag: false, repeatable } }
pub fn flag_option(name: &str) -> CommandOption { CommandOption { name: name.to_owned(), flag: true, repeatable: false } }
type CommandBuilder<I> = Box<dyn Fn(&ParsedCommandInput) -> Result<I, Vec<String>> + Send + Sync>;
type CommandAction<I, C> = Box<dyn Fn(I, C) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync>;
pub struct Command<I, C> {
    pub name: String,
    options: Vec<CommandOption>, subcommands: BTreeMap<String, Command<I, C>>,
    builder: Option<CommandBuilder<I>>, action: Option<CommandAction<I, C>>,
}
impl<I: Clone, C> Command<I, C> {
    pub fn new(name: &str) -> Self { Self { name: name.to_owned(), options: Vec::new(), subcommands: BTreeMap::new(), builder: None, action: None } }
    pub fn option(&mut self, option: CommandOption) -> Result<&mut Self, String> {
        if self.options.iter().any(|existing| existing.name == option.name) { return Err(format!("Option {} is already registered for {}", option.name, self.name)); }
        self.options.push(option); Ok(self)
    }
    pub fn build(&mut self, builder: impl Fn(&ParsedCommandInput) -> Result<I, Vec<String>> + Send + Sync + 'static) -> &mut Self { self.builder = Some(Box::new(builder)); self }
    pub fn action(&mut self, action: impl Fn(I, C) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync + 'static) -> &mut Self { self.action = Some(Box::new(action)); self }
    pub fn command(&mut self, command: Self) -> Result<&mut Self, String> {
        if self.subcommands.contains_key(&command.name) { return Err(format!("Command {} is already registered", command.name)); }
        self.subcommands.insert(command.name.clone(), command); Ok(self)
    }
    fn select<'a>(&'a self, mut argv: &'a [String]) -> (&'a Self, &'a [String]) {
        let mut command = self;
        while let Some(selected) = argv.first().and_then(|name| command.subcommands.get(name)) { command = selected; argv = &argv[1..]; }
        (command, argv)
    }
    fn parse_own(&self, argv: &[String]) -> Result<Result<I, Vec<String>>, String> {
        let builder = self.builder.as_ref().ok_or_else(|| format!("Command {} does not define a builder", self.name))?;
        let mut parsed = parse_options(argv, &self.options);
        let built = builder(&parsed);
        if let Err(errors) = &built { parsed.errors.extend(errors.iter().cloned()); }
        if !parsed.errors.is_empty() { return Ok(Err(parsed.errors)); }
        match built { Ok(invocation) => Ok(Ok(invocation)), Err(_) => Err(format!("Command {} failed without an error", self.name)) }
    }
    pub fn parse(&self, argv: &[String]) -> Result<Result<I, Vec<String>>, String> { let (command, argv) = self.select(argv); command.parse_own(argv) }
    pub async fn execute(&self, argv: &[String], context: C) -> Result<Result<I, Vec<String>>, String> {
        let (command, argv) = self.select(argv);
        let invocation = match command.parse_own(argv)? { Ok(invocation) => invocation, Err(errors) => return Ok(Err(errors)) };
        let action = command.action.as_ref().ok_or_else(|| format!("Command {} does not define an action", command.name))?;
        action(invocation.clone(), context).await?;
        Ok(Ok(invocation))
    }
}
