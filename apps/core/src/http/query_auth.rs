use crate::{http::raw_query::RawQuery, identity::service::PresentedAuth};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryAuthError {
    MissingParameter,
    ConflictingAuthMechanisms,
}

pub(crate) fn presented(query: &RawQuery) -> Result<PresentedAuth, QueryAuthError> {
    let username = required(query, "u")?;
    required(query, "v")?;
    required(query, "c")?;

    let password = query.get("p");
    let token_pair = token_pair(query)?;
    let api_key = query.get("apiKey");

    let provided_mechanisms = [password.is_some(), token_pair.is_some(), api_key.is_some()]
        .into_iter()
        .filter(|present| *present)
        .count();

    if provided_mechanisms > 1 {
        return Err(QueryAuthError::ConflictingAuthMechanisms);
    }

    if let Some(secret) = api_key {
        return Ok(PresentedAuth::ApiKey {
            secret: secret.to_owned(),
        });
    }

    if let Some((token, salt)) = token_pair {
        return Ok(PresentedAuth::Token {
            username: username.to_owned(),
            token: token.to_owned(),
            salt: salt.to_owned(),
        });
    }

    if let Some(value) = password {
        return Ok(PresentedAuth::Password {
            username: username.to_owned(),
            password: decode_password(value).ok_or(QueryAuthError::MissingParameter)?,
        });
    }

    Err(QueryAuthError::MissingParameter)
}

fn required<'a>(query: &'a RawQuery, name: &str) -> Result<&'a str, QueryAuthError> {
    query.get(name).ok_or(QueryAuthError::MissingParameter)
}

// a half-supplied token pair is a malformed request, not a failed login
fn token_pair(query: &RawQuery) -> Result<Option<(&str, &str)>, QueryAuthError> {
    match (query.get("t"), query.get("s")) {
        (Some(token), Some(salt)) => Ok(Some((token, salt))),
        (None, None) => Ok(None),
        _ => Err(QueryAuthError::MissingParameter),
    }
}

// `enc:` marks a hex-encoded password; without it the value is the literal password
fn decode_password(value: &str) -> Option<String> {
    let Some(hex_part) = value.strip_prefix("enc:") else {
        return Some(value.to_owned());
    };

    let bytes = hex::decode(hex_part).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(query: &str) -> RawQuery {
        RawQuery::parse(query)
    }

    #[test]
    fn username_version_and_client_are_required() {
        for missing in ["u", "v", "c"] {
            let query = match missing {
                "u" => parse("v=1.16.1&c=feishin"),
                "v" => parse("u=flynn&c=feishin"),
                _ => parse("u=flynn&v=1.16.1"),
            };

            assert_eq!(
                presented(&query),
                Err(QueryAuthError::MissingParameter),
                "missing {missing} must be rejected"
            );
        }
    }

    #[test]
    fn api_key_mechanism_is_recognized() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&apiKey=whio_abc");

        assert_eq!(
            presented(&query),
            Ok(PresentedAuth::ApiKey {
                secret: "whio_abc".to_owned(),
            })
        );
    }

    #[test]
    fn token_pair_is_recognized() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&t=abc123&s=salt");

        assert_eq!(
            presented(&query),
            Ok(PresentedAuth::Token {
                username: "flynn".to_owned(),
                token: "abc123".to_owned(),
                salt: "salt".to_owned(),
            })
        );
    }

    #[test]
    fn plaintext_password_is_recognized() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=whio_abc");

        assert_eq!(
            presented(&query),
            Ok(PresentedAuth::Password {
                username: "flynn".to_owned(),
                password: "whio_abc".to_owned(),
            })
        );
    }

    #[test]
    fn enc_prefixed_password_is_hex_decoded() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=enc:7768696f5f616263");

        assert_eq!(
            presented(&query),
            Ok(PresentedAuth::Password {
                username: "flynn".to_owned(),
                password: "whio_abc".to_owned(),
            })
        );
    }

    #[test]
    fn enc_prefix_with_invalid_hex_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=enc:zzzz");

        assert_eq!(presented(&query), Err(QueryAuthError::MissingParameter));
    }

    #[test]
    fn enc_prefix_decoding_to_non_utf8_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=enc:ff");

        assert_eq!(presented(&query), Err(QueryAuthError::MissingParameter));
    }

    #[test]
    fn password_and_api_key_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=whio_abc&apiKey=whio_def");

        assert_eq!(
            presented(&query),
            Err(QueryAuthError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn password_and_token_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=whio_abc&t=abc123&s=salt");

        assert_eq!(
            presented(&query),
            Err(QueryAuthError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn token_and_api_key_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&t=abc123&s=salt&apiKey=whio_def");

        assert_eq!(
            presented(&query),
            Err(QueryAuthError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn half_a_token_pair_is_a_grammar_error() {
        let token_only = parse("u=flynn&v=1.16.1&c=feishin&t=abc123");
        let salt_only = parse("u=flynn&v=1.16.1&c=feishin&s=salt");

        assert_eq!(
            presented(&token_only),
            Err(QueryAuthError::MissingParameter)
        );
        assert_eq!(presented(&salt_only), Err(QueryAuthError::MissingParameter));
    }

    #[test]
    fn no_mechanism_at_all_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin");

        assert_eq!(presented(&query), Err(QueryAuthError::MissingParameter));
    }
}
