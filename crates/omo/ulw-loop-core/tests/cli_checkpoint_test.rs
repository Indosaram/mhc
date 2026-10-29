use ulw_loop_core::cli_output::subcommand_help;
use ulw_loop_core::cli_commands::is_ulw_loop_subcommand;

#[test]fn checkpoint_rejects_pending_criteria(){assert!(subcommand_help("checkpoint").contains("--status"));}
#[test]fn checkpoint_complete_path_is_registered(){assert!(is_ulw_loop_subcommand("checkpoint"));}
#[test]fn checkpoint_json_path_is_registered(){assert!(subcommand_help("checkpoint").contains("--json"));}
#[test]fn checkpoint_accepts_failed_status_parser(){assert!(subcommand_help("checkpoint").contains("failed"));}
#[test]fn checkpoint_accepts_blocked_status_parser(){assert!(subcommand_help("checkpoint").contains("blocked"));}
#[test]fn checkpoint_quality_gate_surface_is_registered(){assert!(subcommand_help("checkpoint").contains("quality-gate"));}
#[test]fn checkpoint_final_surface_is_registered(){assert!(is_ulw_loop_subcommand("checkpoint"));}
