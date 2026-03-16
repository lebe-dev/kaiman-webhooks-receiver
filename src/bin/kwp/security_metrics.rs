use kwp_lib::domain::config::model::WebhookChannelConfig;

pub fn record_channel_security_gauges(channels: &[WebhookChannelConfig]) {
    for ch in channels {
        let has_ip_allowlist = ch.allowed_ips.is_some() as u8;
        let has_secret = ch.webhook_secret.is_some() as u8;

        metrics::gauge!(
            "kwp_channel_security_config",
            "channel" => ch.name.clone(),
            "feature" => "ip_allowlist"
        )
        .set(f64::from(has_ip_allowlist));

        metrics::gauge!(
            "kwp_channel_security_config",
            "channel" => ch.name.clone(),
            "feature" => "secret"
        )
        .set(f64::from(has_secret));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kwp_lib::domain::config::model::SecretType;
    use serial_test::serial;

    fn make_channel(name: &str) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "tok".to_string(),
            webhook_secret: None,
            secret_header: None,
            secret_type: SecretType::Plain,
            secret_extract_template: None,
            secret_sign_template: None,
            forward: None,
            max_body_size: None,
            allowed_ips: None,
        }
    }

    #[test]
    #[serial]
    fn security_gauges_reflect_channel_config() {
        let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .unwrap();

        let mut secure = make_channel("secure");
        secure.webhook_secret = Some("s3cret".to_string());
        secure.allowed_ips = Some(vec!["10.0.0.0/8".to_string()]);

        let bare = make_channel("bare");

        record_channel_security_gauges(&[secure, bare]);

        let output = handle.render();

        // secure channel: both features = 1
        assert!(
            output.contains(
                r#"kwp_channel_security_config{channel="secure",feature="ip_allowlist"} 1"#
            )
        );
        assert!(
            output.contains(r#"kwp_channel_security_config{channel="secure",feature="secret"} 1"#)
        );

        // bare channel: both features = 0
        assert!(
            output.contains(
                r#"kwp_channel_security_config{channel="bare",feature="ip_allowlist"} 0"#
            )
        );
        assert!(
            output.contains(r#"kwp_channel_security_config{channel="bare",feature="secret"} 0"#)
        );
    }
}
