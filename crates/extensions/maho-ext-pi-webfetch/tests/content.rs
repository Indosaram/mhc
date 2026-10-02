use maho_ext_pi_webfetch::webfetch::content::decode_html_entities;
#[test]fn named_entities_decode_without_double_decoding(){assert_eq!(decode_html_entities("&amp;lt; &AMP; &unknown;"),"&lt; & &unknown;");}
#[test]fn numeric_entities_decode_unicode(){assert_eq!(decode_html_entities("&#128512; &#x1f600;"),"😀 😀");}
#[test]fn malformed_entities_remain_literal(){assert_eq!(decode_html_entities("&bad&copy; &#X41; &unterminated"),"&bad&copy; &#X41; &unterminated");}
#[test]fn out_of_range_codepoint_is_empty(){assert_eq!(decode_html_entities("before&#1114112;after"),"beforeafter");}
