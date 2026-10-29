use ulw_loop_core::cli_output::subcommand_help;

#[test]fn checkpoint_help_mentions_no_advance(){assert!(subcommand_help("checkpoint").contains("no-advance"));}
#[test]fn checkpoint_help_mentions_json(){assert!(subcommand_help("checkpoint").contains("--json"));}
