use maho_ext_pi_websearch::websearch::provider_endpoints::*;
#[test]fn terminal_dot_private_rejected(){for value in ["https://localhost./search","https://sub.localhost./search","https://127.1../search","https://0177.0.0.1../search","https://2130706433../search","https://0x7f000001../search","https://10.1../search"]{assert!(!is_allowed_provider_base_url(value),"{value}");}}
#[test]fn public_terminal_dot_allowed(){assert!(is_allowed_provider_base_url("https://search-gateway.example.com./search"));}
#[test]fn public_repeated_dot_rejected(){assert!(!is_allowed_provider_base_url("https://search-gateway.example.com../search"));}
