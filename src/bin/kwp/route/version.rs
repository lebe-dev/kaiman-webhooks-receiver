use axum::response::IntoResponse;
use kwp_lib::VERSION;
use reqwest::StatusCode;

pub async fn get_version_route() -> impl IntoResponse {
    (StatusCode::OK, VERSION).into_response()
}
