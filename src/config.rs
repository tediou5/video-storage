use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub listen_on_port: u16,

    #[serde(default = "default_permits")]
    pub permits: usize,

    #[serde(default = "default_token_rate")]
    pub token_rate: f64,

    #[serde(default = "default_workspace")]
    pub workspace: String,

    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_storage_backend")]
    pub backend: String,

    #[serde(default)]
    pub s3: Option<S3Config>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    pub bucket: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,

    #[serde(default = "default_webhook_timeout")]
    pub timeout_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_on_port: default_port(),
            permits: default_permits(),
            token_rate: default_token_rate(),
            workspace: default_workspace(),
            storage: StorageConfig::default(),
            webhook: None,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: default_storage_backend(),
            s3: None,
        }
    }
}

fn default_port() -> u16 {
    32145
}

fn default_permits() -> usize {
    5
}

fn default_token_rate() -> f64 {
    0.0
}

fn default_workspace() -> String {
    ".".to_string()
}

fn default_storage_backend() -> String {
    "local".to_string()
}

fn default_webhook_timeout() -> u64 {
    10
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        // Validate storage configuration
        match self.storage.backend.as_str() {
            "local" => {
                // Local storage doesn't need additional validation
            }
            "s3" => {
                let s3_config = self.storage.s3.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("S3 configuration is required when backend is 's3'")
                })?;

                if s3_config.bucket.is_empty() {
                    return Err(anyhow::anyhow!("S3 bucket name cannot be empty"));
                }
                if s3_config.access_key_id.is_empty() {
                    return Err(anyhow::anyhow!("S3 access key ID cannot be empty"));
                }
                if s3_config.secret_access_key.is_empty() {
                    return Err(anyhow::anyhow!("S3 secret access key cannot be empty"));
                }
            }
            backend => {
                return Err(anyhow::anyhow!(
                    "Unsupported storage backend: {}. Use 'local' or 's3'",
                    backend
                ));
            }
        }

        // Validate webhook configuration
        if let Some(webhook) = &self.webhook {
            if webhook.url.is_empty() {
                return Err(anyhow::anyhow!("Webhook URL cannot be empty"));
            }
            if !webhook.url.starts_with("http://") && !webhook.url.starts_with("https://") {
                return Err(anyhow::anyhow!(
                    "Webhook URL must start with http:// or https://"
                ));
            }
        }

        Ok(())
    }

    /// Merge with command line arguments, CLI args take precedence
    pub fn merge_with_cli(&mut self, args: &crate::Args) {
        // Only override if explicitly provided via CLI
        if args.listen_on_port != default_port() {
            self.listen_on_port = args.listen_on_port;
        }
        if args.permits != default_permits() {
            self.permits = args.permits;
        }
        if args.token_rate != default_token_rate() {
            self.token_rate = args.token_rate;
        }
        if args.workspace != default_workspace() {
            self.workspace = args.workspace.clone();
        }

        // Storage backend override
        if args.storage_backend != default_storage_backend() {
            self.storage.backend = args.storage_backend.clone();
        }

        // S3 configuration override
        if args.storage_backend == "s3" || self.storage.backend == "s3" {
            let mut s3_config = self.storage.s3.clone().unwrap_or(S3Config {
                bucket: String::new(),
                endpoint: None,
                region: None,
                access_key_id: String::new(),
                secret_access_key: String::new(),
            });

            if let Some(bucket) = &args.s3_bucket {
                s3_config.bucket = bucket.clone();
            }
            if args.s3_endpoint.is_some() {
                s3_config.endpoint = args.s3_endpoint.clone();
            }
            if args.s3_region.is_some() {
                s3_config.region = args.s3_region.clone();
            }
            if let Some(key_id) = &args.s3_access_key_id {
                s3_config.access_key_id = key_id.clone();
            }
            if let Some(secret) = &args.s3_secret_access_key {
                s3_config.secret_access_key = secret.clone();
            }

            self.storage.s3 = Some(s3_config);
        }

        // Webhook override
        if let Some(webhook_url) = &args.webhook_url {
            self.webhook = Some(WebhookConfig {
                url: webhook_url.clone(),
                timeout_seconds: default_webhook_timeout(),
            });
        }
    }
}
