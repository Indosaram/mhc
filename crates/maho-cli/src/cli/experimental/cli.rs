use super::commands::{client::{ClientCommand, parse_client}, server::{ServerCommand, parse_server}};
pub enum ExperimentalInvocation { Client(ClientCommand), Server(ServerCommand) }
pub fn parse(argv: &[String]) -> Result<ExperimentalInvocation, Vec<String>> {
    match argv.first().map(String::as_str) {
        Some("client") => parse_client(&argv[1..]).map(ExperimentalInvocation::Client),
        Some("server") => parse_server(&argv[1..]).map(ExperimentalInvocation::Server),
        _ => Err(vec!["Expected experimental command: server or client".to_owned()]),
    }
}
