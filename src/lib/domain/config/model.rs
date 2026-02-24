use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ForwardConfig {
    pub url: String,
    pub interval_seconds: u64,
    #[serde(default = "default_expected_status")]
    pub expected_status: u16,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_expected_status() -> u16 {
    200
}

fn default_timeout_seconds() -> u64 {
    15
}

impl PartialEq for ForwardConfig {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
            && self.interval_seconds == other.interval_seconds
            && self.expected_status == other.expected_status
            && self.timeout_seconds == other.timeout_seconds
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WebhookChannelConfig {
    pub name: String,
    pub api_read_token: String,
    pub webhook_secret: Option<String>,
    pub secret_header: Option<String>,
    pub forward: Option<ForwardConfig>,
    #[serde(default)]
    pub max_body_size: Option<usize>,
}

impl PartialEq for WebhookChannelConfig {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.api_read_token == other.api_read_token
            && self.webhook_secret == other.webhook_secret
            && self.secret_header == other.secret_header
            && self.forward == other.forward
            && self.max_body_size == other.max_body_size
    }
}

const MIN_BODY_LIMIT: usize = 64;
const MAX_BODY_LIMIT: usize = 104_857_600; // 100 MB

#[derive(PartialEq, Clone, Debug)]
pub struct AppConfig {
    pub bind: String,
    pub log_level: String,
    pub log_target: String,
    pub data_path: String,
    pub db_cnn: String,
    pub channels: Vec<WebhookChannelConfig>,
    pub default_body_limit: usize,
}

impl AppConfig {
    /// Constant-time token lookup — for GET (client reads webhooks).
    pub fn find_channel_by_token(&self, bearer: &str) -> Option<&WebhookChannelConfig> {
        self.channels
            .iter()
            .find(|c| c.api_read_token.as_bytes().ct_eq(bearer.as_bytes()).into())
    }

    /// Plain name lookup — for POST (incoming webhook routing).
    pub fn find_channel_by_name(&self, name: &str) -> Option<&WebhookChannelConfig> {
        self.channels.iter().find(|c| c.name == name)
    }

    /// Returns the maximum body limit across all channels and the global default.
    /// Used to set Axum's DefaultBodyLimit layer.
    pub fn max_body_limit(&self) -> usize {
        self.channels
            .iter()
            .filter_map(|c| c.max_body_size)
            .max()
            .unwrap_or(self.default_body_limit)
            .max(self.default_body_limit)
    }

    /// Validates body limit values at startup. Returns Err on invalid config.
    pub fn validate_body_limits(&self) -> Result<(), String> {
        if !(MIN_BODY_LIMIT..=MAX_BODY_LIMIT).contains(&self.default_body_limit) {
            return Err(format!(
                "default_body_limit {} is out of range [{}, {}]",
                self.default_body_limit, MIN_BODY_LIMIT, MAX_BODY_LIMIT
            ));
        }
        for ch in &self.channels {
            if let Some(limit) = ch.max_body_size
                && !(MIN_BODY_LIMIT..=MAX_BODY_LIMIT).contains(&limit)
            {
                return Err(format!(
                    "channel '{}': max-body-size {} is out of range [{}, {}]",
                    ch.name, limit, MIN_BODY_LIMIT, MAX_BODY_LIMIT
                ));
            }
        }
        Ok(())
    }
}

#[derive(PartialEq, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigDto {
    pub bind: String,
    pub log_level: String,
    pub log_target: String,
    pub data_path: String,
}

impl From<AppConfig> for AppConfigDto {
    fn from(config: AppConfig) -> Self {
        AppConfigDto {
            bind: config.bind,
            log_level: config.log_level,
            log_target: config.log_target,
            data_path: config.data_path,
        }
    }
}

#[derive(Debug, Error)]
pub enum LoadAppConfigError {
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app_config(
        default_body_limit: usize,
        channels: Vec<WebhookChannelConfig>,
    ) -> AppConfig {
        AppConfig {
            bind: "0.0.0.0:8080".to_string(),
            log_level: "info".to_string(),
            log_target: "stdout".to_string(),
            data_path: "./data".to_string(),
            db_cnn: "sqlite:test.db".to_string(),
            channels,
            default_body_limit,
        }
    }

    fn make_channel(name: &str, max_body_size: Option<usize>) -> WebhookChannelConfig {
        WebhookChannelConfig {
            name: name.to_string(),
            api_read_token: "token".to_string(),
            webhook_secret: None,
            secret_header: None,
            forward: None,
            max_body_size,
        }
    }

    #[test]
    fn test_channel_config_max_body_size_present() {
        let yaml = r#"
name: test
api-read-token: tok
max-body-size: 1048576
"#;
        let cfg: WebhookChannelConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.max_body_size, Some(1_048_576));
    }

    #[test]
    fn test_channel_config_max_body_size_absent() {
        let yaml = r#"
name: test
api-read-token: tok
"#;
        let cfg: WebhookChannelConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.max_body_size, None);
    }

    #[test]
    fn test_max_body_limit_uses_largest() {
        let channels = vec![
            make_channel("a", Some(1_048_576)), // 1 MB
            make_channel("b", None),
        ];
        let config = make_app_config(262_144, channels); // global = 256 KB
        assert_eq!(config.max_body_limit(), 1_048_576);
    }

    #[test]
    fn test_max_body_limit_no_overrides() {
        let channels = vec![make_channel("a", None)];
        let config = make_app_config(262_144, channels);
        assert_eq!(config.max_body_limit(), 262_144);
    }

    #[test]
    fn test_validate_body_limits_zero_rejected() {
        let channels = vec![make_channel("a", Some(0))];
        let config = make_app_config(262_144, channels);
        assert!(config.validate_body_limits().is_err());
    }

    #[test]
    fn test_validate_body_limits_exceeds_max_rejected() {
        let channels = vec![make_channel("a", Some(200_000_000))]; // 200 MB
        let config = make_app_config(262_144, channels);
        assert!(config.validate_body_limits().is_err());
    }

    #[test]
    fn test_validate_body_limits_valid() {
        let channels = vec![make_channel("a", Some(524_288))];
        let config = make_app_config(262_144, channels);
        assert!(config.validate_body_limits().is_ok());
    }

    #[test]
    fn test_forward_config_deserialization_full() {
        let yaml = r#"
url: https://example.com/hook
interval-seconds: 30
expected-status: 201
timeout-seconds: 10
"#;
        let cfg: ForwardConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.url, "https://example.com/hook");
        assert_eq!(cfg.interval_seconds, 30);
        assert_eq!(cfg.expected_status, 201);
        assert_eq!(cfg.timeout_seconds, 10);
    }

    #[test]
    fn test_forward_config_defaults() {
        let yaml = r#"
url: https://example.com/hook
interval-seconds: 60
"#;
        let cfg: ForwardConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.expected_status, 200);
        assert_eq!(cfg.timeout_seconds, 15);
    }

    #[test]
    fn test_channel_config_with_forward() {
        let yaml = r#"
channels:
  - name: telegram
    api-read-token: abc123
    webhook-secret: mysecret
    secret-header: X-Telegram-Bot-Api-Secret-Token
    forward:
      url: https://my-app.local/telegram-hook
      interval-seconds: 30
  - name: open
    api-read-token: def456
"#;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            channels: Vec<WebhookChannelConfig>,
        }
        let w: Wrapper = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(w.channels.len(), 2);
        assert!(w.channels[0].forward.is_some());
        assert!(w.channels[1].forward.is_none());
        let fwd = w.channels[0].forward.as_ref().unwrap();
        assert_eq!(fwd.url, "https://my-app.local/telegram-hook");
        assert_eq!(fwd.interval_seconds, 30);
        assert_eq!(fwd.expected_status, 200);
    }
}
