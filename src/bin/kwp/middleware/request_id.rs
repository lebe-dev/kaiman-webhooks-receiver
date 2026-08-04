use axum::{extract::Request, middleware::Next, response::Response};

/// Correlates the log records produced while handling one request.
///
/// Without it, two webhooks arriving on the same channel at the same time are
/// indistinguishable in the log: nothing ties the "unauthorized" warning to the
/// request that caused it.
#[derive(Debug, Clone)]
pub struct RequestId(String);

/// Hex characters kept from the generated UUID.
///
/// Full UUIDs would dominate every log line. Eight characters are enough to tell
/// concurrent requests apart while the log is being read; this is a correlation
/// aid, not an identifier anything depends on.
const ID_LENGTH: usize = 8;

impl RequestId {
    fn new() -> Self {
        let uuid = uuid::Uuid::new_v4().simple().to_string();

        Self(uuid[..ID_LENGTH].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Assigns an id to the request and publishes it through the extensions.
///
/// Must be the outermost of the middlewares that log, so every record for the
/// request — including the Sentry scope set by
/// [`crate::middleware::sentry_scope`] — can carry the same id. The id is always
/// generated here rather than taken from a request header: a client-supplied
/// value could repeat, or carry newlines into the log.
pub async fn middleware(mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(RequestId::new());

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_and_hexadecimal() {
        let id = RequestId::new();

        assert_eq!(id.as_str().len(), ID_LENGTH);
        assert!(
            id.as_str().chars().all(|c| c.is_ascii_hexdigit()),
            "log lines are read by humans: got '{id}'"
        );
    }

    #[test]
    fn concurrent_requests_get_different_ids() {
        let ids: std::collections::HashSet<String> =
            (0..1_000).map(|_| RequestId::new().0).collect();

        assert_eq!(
            ids.len(),
            1_000,
            "an id that repeats cannot correlate anything"
        );
    }
}
