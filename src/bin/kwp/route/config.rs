use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::AppState;
use crate::middleware::request_id::RequestId;
use kwp_lib::domain::config::model::AppConfigPublicDto;

pub async fn get_config_route(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let bearer = match bearer {
        Some(b) => b,
        None => {
            log::warn!(
                "[req:{}] missing or invalid Authorization header for config request",
                request_id
            );
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    };

    let authorized =
        state.config.find_channel_by_token(bearer).is_some() || state.config.is_ui_token(bearer);
    if !authorized {
        log::warn!("[req:{}] invalid token for config request", request_id);
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    // The UI fetches this on every page load and it changes nothing, so the success
    // path stays out of the INFO narrative.
    log::debug!("[req:{}] served public configuration", request_id);

    let dto = AppConfigPublicDto::from(&state.config);
    (StatusCode::OK, Json(dto)).into_response()
}
