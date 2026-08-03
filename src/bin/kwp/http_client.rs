use std::sync::OnceLock;

/// Installs the ring crypto provider for rustls.
///
/// `reqwest` is built with the `rustls-no-provider` feature, so the process-wide
/// provider must be selected explicitly before the first TLS client is created.
fn install_crypto_provider() {
    static INSTALLED: OnceLock<()> = OnceLock::new();

    INSTALLED.get_or_init(|| {
        // Err means another caller won the race — the provider is installed either way.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Builds the HTTP client used for forwarding webhooks.
///
/// Root certificates come from the host trust store, so `ca-certificates`
/// must be present in the runtime image.
pub fn build_http_client() -> reqwest::Result<reqwest::Client> {
    install_crypto_provider();

    reqwest::Client::builder().build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn https_scheme_is_supported() {
        let client = build_http_client().expect("client must build");

        // Port 1 is closed: a TLS-capable client fails while connecting, while a
        // client without any TLS backend rejects the URL scheme outright.
        let err = client
            .post("https://127.0.0.1:1/hook")
            .body("payload")
            .send()
            .await
            .expect_err("connection to a closed port must fail");

        assert!(
            !err.to_string().contains("scheme is not http"),
            "https must be supported, got: {err}"
        );
        assert!(err.is_connect(), "expected a connect error, got: {err}");
    }
}
