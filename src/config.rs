use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Main configuration structure that can be loaded from CLI, config file, or environment
///
/// Example configuration file content
/// # Video Storage Configuration
///
/// # Server configuration
/// listen_on_port = 32145
/// permits = 5
/// token_rate = 0.0
/// workspace = "./data"
///
/// # Storage configuration
/// storage_backend = "s3"  # Options: "local" or "s3"
///
/// # S3 configuration (required when storage_backend = "s3")
/// s3_bucket = "my-video-bucket"
/// s3_endpoint = "http://localhost:9000"  # Optional: for MinIO or custom S3
/// s3_region = "us-east-1"                # Optional
/// s3_access_key_id = "minioadmin"
/// s3_secret_access_key = "minioadmin"
///
/// # Webhook configuration (optional)
/// webhook_url = "https://example.com/webhook"
#[derive(Debug, Clone, Serialize, Deserialize, Parser)]
#[command(version, about, long_about = None)]
#[serde(default)]
pub struct Config {
    /// Port to listen on
    #[arg(short, long, default_value_t = 32145)]
    #[serde(default = "default_port")]
    pub listen_on_port: u16,

    /// Number of concurrent conversion jobs
    #[arg(short, long, default_value_t = 5)]
    #[serde(default = "default_permits")]
    pub permits: usize,

    /// Token bucket rate limiting (0.0 = disabled)
    #[arg(short, long, default_value_t = 0.0)]
    #[serde(default = "default_token_rate")]
    pub token_rate: f64,

    /// Working directory for file storage
    #[arg(short = 'w', long, default_value = ".")]
    #[serde(default = "default_workspace")]
    pub workspace: String,

    /// Configuration file path (overrides all other arguments)
    #[arg(short, long)]
    #[serde(skip)]
    pub config: Option<String>,

    /// Storage backend: local or s3
    #[arg(short, long, default_value = "local")]
    #[serde(default = "default_storage_backend")]
    pub storage_backend: String,

    /// S3 bucket name (required when storage-backend is s3)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3_bucket: Option<String>,

    /// S3 endpoint (for MinIO/custom S3)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3_endpoint: Option<String>,

    /// S3 region
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3_region: Option<String>,

    /// S3 access key ID
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3_access_key_id: Option<String>,

    /// S3 secret access key
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3_secret_access_key: Option<String>,

    /// Webhook URL to call when jobs complete
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_on_port: default_port(),
            permits: default_permits(),
            token_rate: default_token_rate(),
            workspace: default_workspace(),
            config: None,
            storage_backend: default_storage_backend(),
            s3_bucket: None,
            s3_endpoint: None,
            s3_region: None,
            s3_access_key_id: None,
            s3_secret_access_key: None,
            webhook_url: None,
        }
    }
}

impl Config {
    /// Load configuration from CLI args, optionally merging with a config file
    pub fn load() -> Result<Self> {
        // First parse CLI args
        let mut config = Config::parse();

        // If a config file is specified, load it and merge
        if let Some(config_path) = &config.config {
            let file_config = Self::from_file(Path::new(config_path))?;
            config = config.merge_with_file(file_config);
        }

        config.validate()?;
        Ok(config)
    }

    /// Load configuration from a TOML file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Merge with file config, CLI args take precedence
    fn merge_with_file(mut self, file_config: Config) -> Self {
        // Use file config as base, but override with CLI args if they're not default

        // If CLI value is default, use file value
        if self.listen_on_port == default_port() {
            self.listen_on_port = file_config.listen_on_port;
        }
        if self.permits == default_permits() {
            self.permits = file_config.permits;
        }
        if self.token_rate == default_token_rate() {
            self.token_rate = file_config.token_rate;
        }
        if self.workspace == default_workspace() {
            self.workspace = file_config.workspace;
        }
        if self.storage_backend == default_storage_backend() {
            self.storage_backend = file_config.storage_backend;
        }

        // For Option fields, CLI takes precedence if Some
        if self.s3_bucket.is_none() {
            self.s3_bucket = file_config.s3_bucket;
        }
        if self.s3_endpoint.is_none() {
            self.s3_endpoint = file_config.s3_endpoint;
        }
        if self.s3_region.is_none() {
            self.s3_region = file_config.s3_region;
        }
        if self.s3_access_key_id.is_none() {
            self.s3_access_key_id = file_config.s3_access_key_id;
        }
        if self.s3_secret_access_key.is_none() {
            self.s3_secret_access_key = file_config.s3_secret_access_key;
        }
        if self.webhook_url.is_none() {
            self.webhook_url = file_config.webhook_url;
        }

        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        // Validate storage configuration
        match self.storage_backend.as_str() {
            "local" => {
                // Local storage doesn't need additional validation
            }
            "s3" => {
                if self
                    .s3_bucket
                    .as_ref()
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                {
                    return Err(anyhow::anyhow!(
                        "S3 bucket name is required when backend is 's3'"
                    ));
                }
                if self
                    .s3_access_key_id
                    .as_ref()
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                {
                    return Err(anyhow::anyhow!(
                        "S3 access key ID is required when backend is 's3'"
                    ));
                }
                if self
                    .s3_secret_access_key
                    .as_ref()
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                {
                    return Err(anyhow::anyhow!(
                        "S3 secret access key is required when backend is 's3'"
                    ));
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
        if let Some(webhook_url) = &self.webhook_url {
            if webhook_url.is_empty() {
                return Err(anyhow::anyhow!("Webhook URL cannot be empty"));
            }
            if !webhook_url.starts_with("http://") && !webhook_url.starts_with("https://") {
                return Err(anyhow::anyhow!(
                    "Webhook URL must start with http:// or https://"
                ));
            }
        }

        Ok(())
    }

    /// Convert to S3 storage backend configuration
    pub fn to_s3_config(&self) -> Option<S3Config> {
        if self.storage_backend != "s3" {
            return None;
        }

        Some(S3Config {
            bucket: self.s3_bucket.clone()?,
            endpoint: self.s3_endpoint.clone(),
            region: self.s3_region.clone(),
            access_key_id: self.s3_access_key_id.clone()?,
            secret_access_key: self.s3_secret_access_key.clone()?,
        })
    }
}

// S3 configuration subset
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
}

// Default value functions
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
