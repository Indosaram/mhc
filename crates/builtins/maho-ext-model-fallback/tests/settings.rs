use maho_ext_api::FlagValue;
use maho_ext_model_fallback::settings::is_model_fallback_disabled;
#[test]
fn flag_and_environment_override(){
    for (flag,environment,expected) in [(Some(FlagValue::Boolean(true)),None,true),(Some(FlagValue::Boolean(false)),Some("1"),true),(None,None,false),(Some(FlagValue::String("true".into())),None,false),(Some(FlagValue::Boolean(false)),Some("true"),false)]{assert_eq!(is_model_fallback_disabled(flag.as_ref(),environment),expected);}
}
