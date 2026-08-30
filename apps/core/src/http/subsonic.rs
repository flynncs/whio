use axum::{
    Json,
    extract::FromRequestParts,
    http::request::Parts,
    response::{IntoResponse, Response as AxumResponse},
};
use whio_subsonic_api::models;

use crate::{http::raw_query::RawQuery, identity::service::PresentedAuth};

pub(crate) const SUBSONIC_API_VERSION: &str = "1.16.1";
const SERVER_TYPE: &str = "whio";

pub(crate) struct SubsonicAuth(pub(crate) PresentedAuth);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubsonicError {
    MissingParameter,
    ConflictingAuthMechanisms,
}

impl SubsonicError {
    fn code(self) -> models::error::Code {
        match self {
            Self::MissingParameter => models::error::Code::Variant10,
            Self::ConflictingAuthMechanisms => models::error::Code::Variant43,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::MissingParameter => "Required parameter is missing.",
            Self::ConflictingAuthMechanisms => {
                "Multiple conflicting authentication mechanisms provided."
            }
        }
    }
}

impl IntoResponse for SubsonicError {
    fn into_response(self) -> AxumResponse {
        Json(failed_envelope(self.code(), self.message())).into_response()
    }
}

impl<S> FromRequestParts<S> for SubsonicAuth
where
    S: Send + Sync,
{
    type Rejection = SubsonicError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = RawQuery::from_request_parts(parts, _state)
            .await
            .unwrap_or_else(|infallible| match infallible {});

        extract_auth(&query).map(Self)
    }
}

fn extract_auth(query: &RawQuery) -> Result<PresentedAuth, SubsonicError> {
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
        return Err(SubsonicError::ConflictingAuthMechanisms);
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
            password: decode_password(value).ok_or(SubsonicError::MissingParameter)?,
        });
    }

    Err(SubsonicError::MissingParameter)
}

fn required<'a>(query: &'a RawQuery, name: &str) -> Result<&'a str, SubsonicError> {
    query.get(name).ok_or(SubsonicError::MissingParameter)
}

// a half-supplied token pair is a malformed request, not a failed login
fn token_pair(query: &RawQuery) -> Result<Option<(&str, &str)>, SubsonicError> {
    match (query.get("t"), query.get("s")) {
        (Some(token), Some(salt)) => Ok(Some((token, salt))),
        (None, None) => Ok(None),
        _ => Err(SubsonicError::MissingParameter),
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

pub(crate) fn ok_envelope() -> models::SubsonicResponse {
    models::SubsonicResponse {
        subsonic_response: Some(Box::new(
            models::SubsonicResponseSubsonicResponse::SubsonicSuccessResponse(Box::new(
                models::SubsonicSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::subsonic_success_response::Status::Ok,
                },
            )),
        )),
    }
}

pub(crate) fn failed_envelope(
    code: models::error::Code,
    message: &str,
) -> models::SubsonicResponse {
    models::SubsonicResponse {
        subsonic_response: Some(Box::new(
            models::SubsonicResponseSubsonicResponse::SubsonicFailureResponse(Box::new(
                models::SubsonicFailureResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::subsonic_failure_response::Status::Failed,
                    error: Box::new(models::Error {
                        code,
                        message: Some(message.to_owned()),
                        help_url: None,
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn extensions_envelope() -> models::GetOpenSubsonicExtensionsResponse {
    models::GetOpenSubsonicExtensionsResponse {
        subsonic_response: Some(Box::new(
            models::GetOpenSubsonicExtensionsResponseSubsonicResponse::
                GetOpenSubsonicExtensionsSuccessResponse(Box::new(
                    models::GetOpenSubsonicExtensionsSuccessResponse {
                        version: SUBSONIC_API_VERSION.to_owned(),
                        r#type: SERVER_TYPE.to_owned(),
                        server_version: env!("CARGO_PKG_VERSION").to_owned(),
                        open_subsonic: true,
                        status: models::get_open_subsonic_extensions_success_response::Status::Ok,
                        open_subsonic_extensions: Vec::new(),
                    },
                )),
        )),
    }
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
                extract_auth(&query),
                Err(SubsonicError::MissingParameter),
                "missing {missing} must be rejected"
            );
        }
    }

    #[test]
    fn api_key_mechanism_is_recognized() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&apiKey=whio_abc");

        assert_eq!(
            extract_auth(&query),
            Ok(PresentedAuth::ApiKey {
                secret: "whio_abc".to_owned(),
            })
        );
    }

    #[test]
    fn token_pair_is_recognized() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&t=abc123&s=salt");

        assert_eq!(
            extract_auth(&query),
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
            extract_auth(&query),
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
            extract_auth(&query),
            Ok(PresentedAuth::Password {
                username: "flynn".to_owned(),
                password: "whio_abc".to_owned(),
            })
        );
    }

    #[test]
    fn enc_prefix_with_invalid_hex_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=enc:zzzz");

        assert_eq!(extract_auth(&query), Err(SubsonicError::MissingParameter));
    }

    #[test]
    fn enc_prefix_decoding_to_non_utf8_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=enc:ff");

        assert_eq!(extract_auth(&query), Err(SubsonicError::MissingParameter));
    }

    #[test]
    fn password_and_api_key_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=whio_abc&apiKey=whio_def");

        assert_eq!(
            extract_auth(&query),
            Err(SubsonicError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn password_and_token_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&p=whio_abc&t=abc123&s=salt");

        assert_eq!(
            extract_auth(&query),
            Err(SubsonicError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn token_and_api_key_conflict() {
        let query = parse("u=flynn&v=1.16.1&c=feishin&t=abc123&s=salt&apiKey=whio_def");

        assert_eq!(
            extract_auth(&query),
            Err(SubsonicError::ConflictingAuthMechanisms)
        );
    }

    #[test]
    fn half_a_token_pair_is_a_grammar_error() {
        let token_only = parse("u=flynn&v=1.16.1&c=feishin&t=abc123");
        let salt_only = parse("u=flynn&v=1.16.1&c=feishin&s=salt");

        assert_eq!(
            extract_auth(&token_only),
            Err(SubsonicError::MissingParameter)
        );
        assert_eq!(
            extract_auth(&salt_only),
            Err(SubsonicError::MissingParameter)
        );
    }

    #[test]
    fn no_mechanism_at_all_is_a_grammar_error() {
        let query = parse("u=flynn&v=1.16.1&c=feishin");

        assert_eq!(extract_auth(&query), Err(SubsonicError::MissingParameter));
    }

    #[test]
    fn grammar_errors_map_to_spec_error_codes() {
        assert_eq!(
            SubsonicError::MissingParameter.code(),
            models::error::Code::Variant10
        );
        assert_eq!(
            SubsonicError::ConflictingAuthMechanisms.code(),
            models::error::Code::Variant43
        );
    }
}
