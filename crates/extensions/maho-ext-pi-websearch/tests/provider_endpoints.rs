use maho_ext_pi_websearch::websearch::provider_endpoints::*;
#[test]fn pinned_endpoint_boundary_corpus(){let cases:serde_json::Value=serde_json::from_str(include_str!("fixtures/pinned-endpoint-boundaries.json")).unwrap();for case in cases.as_array().unwrap(){let input=case["input"].as_str().unwrap();assert_eq!(is_allowed_provider_base_url(input),case["allowed"].as_bool().unwrap(),"{input}");}}
#[test]fn terminal_dot_private_rejected(){for value in ["https://localhost./search","https://sub.localhost./search","https://127.1../search","https://0177.0.0.1../search","https://2130706433../search","https://0x7f000001../search","https://10.1../search"]{assert!(!is_allowed_provider_base_url(value),"{value}");}}
#[test]fn public_terminal_dot_allowed(){assert!(is_allowed_provider_base_url("https://search-gateway.example.com./search"));}
#[test]fn public_repeated_dot_rejected(){assert!(!is_allowed_provider_base_url("https://search-gateway.example.com../search"));}
