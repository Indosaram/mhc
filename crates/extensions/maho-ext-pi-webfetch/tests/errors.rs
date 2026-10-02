use maho_ext_pi_webfetch::webfetch::errors::WebfetchError;
#[test]fn source_error_class_names_are_preserved(){for (error,name) in [(WebfetchError::InvalidUrl("fixture".into()),"InvalidWebfetchUrlError"),(WebfetchError::Aborted,"WebfetchAbortError"),(WebfetchError::Timeout(3),"WebfetchTimeoutError"),(WebfetchError::ResponseTooLarge,"WebfetchResponseTooLargeError")]{assert_eq!(error.name(),name);}}
