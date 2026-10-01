//! Port of senpi packages/ai/src/auth/oauth/authorization-input.ts.

use url::Url;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthorizationInput {
    pub code: Option<String>,
    pub state: Option<String>,
}

pub fn parse_authorization_input(input: &str) -> AuthorizationInput {
    let value = input.trim();
    if value.is_empty() {
        return AuthorizationInput::default();
    }

    if let Ok(url) = Url::parse(value) {
        let mut code = None;
        let mut state = None;
        for (key, val) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(val.into_owned()),
                "state" => state = Some(val.into_owned()),
                _ => {}
            }
        }
        return AuthorizationInput { code, state };
    }

    if value.contains('#') {
        let mut parts = value.split('#');
        let code = parts.next().map(str::to_string);
        let state = parts.next().map(str::to_string);
        return AuthorizationInput { code, state };
    }

    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for (key, val) in url::form_urlencoded::parse(value.as_bytes()) {
            match key.as_ref() {
                "code" => code = Some(val.into_owned()),
                "state" => state = Some(val.into_owned()),
                _ => {}
            }
        }
        return AuthorizationInput { code, state };
    }

    AuthorizationInput { code: Some(value.to_string()), state: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected(code: Option<&str>, state: Option<&str>) -> AuthorizationInput {
        AuthorizationInput { code: code.map(str::to_string), state: state.map(str::to_string) }
    }

    #[test]
    fn blank_input_parses_to_nothing() {
        assert_eq!(parse_authorization_input(""), AuthorizationInput::default());
        assert_eq!(parse_authorization_input("   "), AuthorizationInput::default());
    }

    #[test]
    fn absolute_redirect_url_parses_code_and_state() {
        let parsed = parse_authorization_input("http://localhost:53692/callback?code=abc&state=xyz");
        assert_eq!(parsed, expected(Some("abc"), Some("xyz")));
    }

    #[test]
    fn hash_pair_parses_code_and_state() {
        assert_eq!(parse_authorization_input("abc#xyz"), expected(Some("abc"), Some("xyz")));
    }

    #[test]
    fn query_string_without_url_parses_both_params() {
        assert_eq!(parse_authorization_input("code=abc&state=xyz"), expected(Some("abc"), Some("xyz")));
        assert_eq!(parse_authorization_input("state=xyz&code=abc"), expected(Some("abc"), Some("xyz")));
    }

    #[test]
    fn bare_code_is_returned_as_the_code() {
        assert_eq!(parse_authorization_input("plain-code"), expected(Some("plain-code"), None));
        assert_eq!(parse_authorization_input("  plain-code  "), expected(Some("plain-code"), None));
    }

    #[test]
    fn url_without_params_yields_no_code_or_state() {
        assert_eq!(parse_authorization_input("https://example.com/callback"), AuthorizationInput::default());
    }
}
