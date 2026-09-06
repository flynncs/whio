use aws_lc_rs::rand;
use axum::{
    Extension, Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use md5::{Digest, Md5};

use crate::{
    AppState,
    identity::service::{AuthError, PresentedAuth},
    request::RequestContext,
};

use super::{
    dto::{AuthPayload, LoginBody},
    errors, session,
};

pub(crate) struct NdToken(pub(crate) String);

impl FromRequestParts<AppState> for NdToken {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, Response> {
        match parts
            .headers
            .get("x-nd-authorization")
            .and_then(|v| v.to_str().ok())
        {
            Some(value) if value.len() > 7 && value[..6].eq_ignore_ascii_case("bearer") => {
                Ok(Self(value[7..].to_string()))
            }
            _ => Err(errors::error(StatusCode::UNAUTHORIZED, "Not authenticated")),
        }
    }
}

pub(crate) async fn login(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Json(body): Json<LoginBody>,
) -> Response {
    let presented = PresentedAuth::Password {
        username: body.username.clone(),
        password: body.password.clone(),
    };

    let result = state.credential().authenticate(&presented).await;

    match result {
        Ok(principal) => {
            let user = match state.credential().find_user(&body.username).await {
                Ok(Some(user)) => user,
                Ok(None) => {
                    return errors::error(StatusCode::UNAUTHORIZED, "Invalid username or password");
                }
                Err(error) => {
                    tracing::error!(error = %error, request_id = context.request_id().as_str(), "credential storage failed");
                    return errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.");
                }
            };

            let mut salt_bytes = [0u8; 3];
            rand::fill(&mut salt_bytes).expect("system rng");
            let salt = hex::encode(salt_bytes);

            let mut hasher = Md5::new();
            hasher.update(body.password.as_bytes());
            hasher.update(salt.as_bytes());
            let token: String = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();

            Json(AuthPayload {
                id: user.id.to_string(),
                name: user.display_name,
                username: user.username,
                is_admin: false,
                token: session::mint(principal.user_id, state.credential().key()),
                subsonic_salt: salt,
                subsonic_token: token,
            })
            .into_response()
        }
        Err(AuthError::InvalidCredentials) => {
            errors::error(StatusCode::UNAUTHORIZED, "Invalid username or password")
        }
        Err(AuthError::Storage(error)) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "credential storage failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
