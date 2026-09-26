use crate::{features::identity::service::PresentedAuth, platform::raw_query::RawQuery};

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
