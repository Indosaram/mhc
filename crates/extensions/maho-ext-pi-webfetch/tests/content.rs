use maho_ext_pi_webfetch::webfetch::content::decode_html_entities;
#[test]fn named_entities_decode_without_double_decoding(){assert_eq!(decode_html_entities("&amp;lt; &AMP; &unknown;"),"&lt; & &unknown;");}
#[test]fn numeric_entities_decode_unicode(){assert_eq!(decode_html_entities("&#128512; &#x1f600;"),"😀 😀");}
#[test]fn malformed_entities_remain_literal(){assert_eq!(decode_html_entities("&bad&copy; &#X41; &unterminated"),"&bad&copy; &#X41; &unterminated");}
#[test]fn out_of_range_codepoint_is_empty(){assert_eq!(decode_html_entities("before&#1114112;after"),"beforeafter");}
#[test]fn plain_text_collapses_horizontal_whitespace_and_preserves_cr(){use maho_ext_pi_webfetch::webfetch::content::normalize_plain_text;assert_eq!(normalize_plain_text("\u{feff} a\t\u{000b}\u{00a0}b  \n  c\n\n\n\n d\r\ne "),"a b\nc\n\nd\r\ne");}
#[test]fn markdown_normalizes_cr_without_collapsing_inline_spacing(){use maho_ext_pi_webfetch::webfetch::content::normalize_markdown;assert_eq!(normalize_markdown("  # Title\r\n\r\n\r\n  a  b \t\r c\t "),"# Title\n\na  b\nc");}
