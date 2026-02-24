use std::env;
use std::path::Path;

use serde::Deserialize;

use crate::domain::config::model::{AppConfig, LoadAppConfigError, WebhookChannelConfig};
use crate::domain::config::ports::AppConfigLoader;

const DEFAULT_BODY_LIMIT_BYTES: usize = 262_144; // 256 KB

#[derive(Deserialize)]
struct WebhookChannelsConfig {
    channels: Vec<WebhookChannelConfig>,
}

#[derive(Clone)]
pub struct EnvConfigLoader;

impl AppConfigLoader for EnvConfigLoader {
    fn load(&self) -> Result<AppConfig, LoadAppConfigError> {
        let bind = env::var("BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let log_target = env::var("LOG_TARGET").unwrap_or_else(|_| "stdout".to_string());

        let data_path = env::var("DATA_PATH").unwrap_or_else(|_| "./data".to_string());

        let db_cnn = env::var("DATABASE_URL").map_err(|_| {
            LoadAppConfigError::Unknown(anyhow::anyhow!("DATABASE_URL is required"))
        })?;

        let config_path = env::var("CONFIG_FILE").unwrap_or_else(|_| "config.yml".to_string());

        let app_config = load_channels_from_file(&config_path)?;

        let default_body_limit: usize = match env::var("DEFAULT_BODY_LIMIT") {
            Ok(val) => match val.parse() {
                Ok(v) => v,
                Err(_) => {
                    log::warn!(
                        "invalid DEFAULT_BODY_LIMIT value '{}', using default {}",
                        val,
                        DEFAULT_BODY_LIMIT_BYTES
                    );
                    DEFAULT_BODY_LIMIT_BYTES
                }
            },
            Err(_) => DEFAULT_BODY_LIMIT_BYTES,
        };

        Ok(AppConfig {
            bind,
            log_level,
            log_target,
            data_path,
            db_cnn,
            channels: app_config.channels,
            default_body_limit,
        })
    }
}

fn load_channels_from_file<P: AsRef<Path>>(
    path: P,
) -> Result<WebhookChannelsConfig, LoadAppConfigError> {
    let path = path.as_ref();

    let content = std::fs::read_to_string(path).map_err(|e| {
        LoadAppConfigError::Unknown(anyhow::anyhow!(
            "failed to read config file '{}': {}",
            path.display(),
            e
        ))
    })?;

    config::Config::builder()
        .add_source(config::File::from_str(&content, config::FileFormat::Yaml))
        .build()
        .map_err(|e| {
            LoadAppConfigError::Unknown(anyhow::anyhow!("failed to parse config file: {}", e))
        })?
        .try_deserialize::<WebhookChannelsConfig>()
        .map_err(|e| {
            LoadAppConfigError::Unknown(anyhow::anyhow!("failed to deserialize config: {}", e))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    fn test_config_path() -> String {
        "test-data/config.yml".to_string()
    }

    #[test]
    #[serial]
    fn test_env_loader_minimal_success() {
        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", test_config_path());

            // Clean up optional vars to test defaults
            env::remove_var("BIND_ADDRESS");
            env::remove_var("LOG_LEVEL");
            env::remove_var("LOG_TARGET");
            env::remove_var("DATA_PATH");
        }

        let loader = EnvConfigLoader;
        let config = loader.load().unwrap();

        assert_eq!(config.db_cnn, "sqlite:test.db");
        assert_eq!(config.bind, "0.0.0.0:8080"); // default
        assert!(config.channels.len() >= 1);
        assert_eq!(config.channels[0].name, "test");

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_env_loader_full_success() {
        unsafe {
            env::set_var("BIND_ADDRESS", "127.0.0.1:9000");
            env::set_var("LOG_LEVEL", "debug");
            env::set_var("LOG_TARGET", "file");
            env::set_var("DATA_PATH", "/tmp/data");
            env::set_var("DATABASE_URL", "sqlite:full.db");
            env::set_var("CONFIG_FILE", test_config_path());
        }

        let loader = EnvConfigLoader;
        let config = loader.load().unwrap();

        assert_eq!(config.bind, "127.0.0.1:9000");
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.log_target, "file");
        assert_eq!(config.data_path, "/tmp/data");
        assert_eq!(config.db_cnn, "sqlite:full.db");
        assert_eq!(config.channels[0].name, "test");

        unsafe {
            env::remove_var("BIND_ADDRESS");
            env::remove_var("LOG_LEVEL");
            env::remove_var("LOG_TARGET");
            env::remove_var("DATA_PATH");
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_env_loader_missing_required() {
        unsafe {
            env::remove_var("DATABASE_URL");
            env::set_var("CONFIG_FILE", "/nonexistent/config.yml");
        }

        let loader = EnvConfigLoader;
        // Missing DATABASE_URL is checked first
        assert!(loader.load().is_err());

        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
        }
        // Now config file doesn't exist
        assert!(loader.load().is_err());

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_env_loader_missing_config_file() {
        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", "/nonexistent/config.yml");
        }

        let loader = EnvConfigLoader;
        assert!(loader.load().is_err());

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_env_loader_invalid_yaml() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.yml");

        fs::write(&config_path, "invalid yaml: [").unwrap();

        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", config_path.to_str().unwrap());
        }

        let loader = EnvConfigLoader;
        assert!(loader.load().is_err());

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_default_body_limit_no_env() {
        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", test_config_path());
            env::remove_var("DEFAULT_BODY_LIMIT");
        }

        let loader = EnvConfigLoader;
        let config = loader.load().unwrap();
        assert_eq!(config.default_body_limit, 262_144);

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
        }
    }

    #[test]
    #[serial]
    fn test_default_body_limit_valid_env() {
        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", test_config_path());
            env::set_var("DEFAULT_BODY_LIMIT", "524288");
        }

        let loader = EnvConfigLoader;
        let config = loader.load().unwrap();
        assert_eq!(config.default_body_limit, 524_288);

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
            env::remove_var("DEFAULT_BODY_LIMIT");
        }
    }

    #[test]
    #[serial]
    fn test_default_body_limit_invalid_env_falls_back() {
        unsafe {
            env::set_var("DATABASE_URL", "sqlite:test.db");
            env::set_var("CONFIG_FILE", test_config_path());
            env::set_var("DEFAULT_BODY_LIMIT", "not-a-number");
        }

        let loader = EnvConfigLoader;
        let config = loader.load().unwrap();
        assert_eq!(config.default_body_limit, 262_144);

        unsafe {
            env::remove_var("DATABASE_URL");
            env::remove_var("CONFIG_FILE");
            env::remove_var("DEFAULT_BODY_LIMIT");
        }
    }
}
