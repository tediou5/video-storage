use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::ArgAction::Append;
use clap::Parser;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Main configuration structure that can be loaded from CLI, config file, or environment
///
/// Example configuration file content
/// # Video Storage Configuration
///
/// # Server configuration
/// listen_on_port = 32145
/// internal_port = 32146
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
#[serde(default)]
#[command(version, about, long_about = None)]
pub struct Config {
    /// Port to external API listen on
    #[arg(short, long, default_value_t = 32145)]
    #[serde(default = "default_port")]
    pub listen_on_port: u16,

    /// Internal API port to listen on
    #[arg(long, default_value_t = 32146)]
    #[serde(default = "default_internal_port")]
    pub internal_port: u16,

    /// Number of concurrent conversion jobs
    #[arg(short, long, default_value_t = 5)]
    #[serde(default = "default_permits")]
    pub permits: usize,

    /// (Deprecated): Token bucket rate limiting (0.0 = disabled)
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

    /// Claim signing keys configuration (kid -> base64 encoded 32-byte key).
    /// Can be specified multiple times as --claim-key 1:base64key.
    /// You can generate a key with: openssl rand -base64 32
    #[arg(long = "claim-key", value_parser = parse_claim_key, action = Append)]
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de_claim_keys"
    )]
    pub claim_keys: Vec<(u8, [u8; 32])>,
}

/// Parse claim key from command line format "kid:base64_key"
fn parse_claim_key(s: &str) -> Result<(u8, [u8; 32]), String> {
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err("Invalid format. Use kid:base64_key".to_string());
    }

    // Validate kid is a valid u8
    let kid = parts[0];
    let kid = kid
        .parse::<u8>()
        .map_err(|_| format!("Invalid kid '{kid}'. Must be a number between 0-255"))?;

    let key_bytes = STANDARD
        .decode(parts[1])
        .map_err(|error| format!("Failed to decode base64 key for {kid}: {error}"))?;

    // Validate key length
    if key_bytes.len() != 32 {
        return Err(format!(
            "Invalid key length for kid {kid}: expected 32 bytes, got {}",
            key_bytes.len()
        ));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);

    Ok((kid, key))
}

fn de_claim_keys<'de, D>(de: D) -> Result<Vec<(u8, [u8; 32])>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut out: Vec<(u8, [u8; 32])> = Vec::new();

    let repr = Option::<HashMap<String, String>>::deserialize(de)?;
    let Some(repr) = repr else {
        return Ok(out);
    };

    for (kstr, v) in repr {
        let kid: u8 = kstr.parse().map_err(serde::de::Error::custom)?;
        let bytes = STANDARD.decode(v).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "claim_keys[{kid}] length {}, expect 32",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        out.push((kid, arr));
    }

    // 可选：按 kid 排序，去重（例如“后出现的覆盖前面”）
    out.sort_unstable_by_key(|(k, _)| *k);
    out.dedup_by_key(|(k, _)| *k);

    Ok(out)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_on_port: default_port(),
            internal_port: default_internal_port(),
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
            claim_keys: Vec::new(),
        }
    }
}

impl Config {
    /// Load configuration from CLI args, optionally merging with a config file
    pub fn load() -> anyhow::Result<Self> {
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
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
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
        if self.internal_port == default_internal_port() {
            self.internal_port = file_config.internal_port;
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
        if self.claim_keys.is_empty() {
            self.claim_keys = file_config.claim_keys;
        }

        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
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

    pub fn get_claim_key(&self, kid: u8) -> Option<[u8; 32]> {
        self.claim_keys
            .iter()
            .find(|(k, _)| *k == kid)
            .map(|(_, key)| *key)
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

fn default_internal_port() -> u16 {
    32146
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_args_with_claim_keys_from_cli() {
        let key1 = STANDARD.encode([3u8; 32]);

        let cli_content = [
            "CLI".to_string(),
            "--listen-on-port".to_string(),
            "8080".to_string(),
            "--internal-port".to_string(),
            "8081".to_string(),
            "--permits".to_string(),
            "10".to_string(),
            "--token-rate".to_string(),
            "5.0".to_string(),
            "--workspace".to_string(),
            "/tmp/test".to_string(),
            "--storage-backend".to_string(),
            "local".to_string(),
            "--claim-key".to_string(),
            format!("1:{key1}"),
            "--claim-key".to_string(),
            "2:uBhfVeH0b7KQKfwOJqhwzLXKBpg7xLPBe5HjCksDDWg=".to_string(),
        ];

        let config = Config::try_parse_from(cli_content).unwrap();

        assert_eq!(config.listen_on_port, 8080);
        assert!(!config.claim_keys.is_empty());

        assert_eq!(config.claim_keys.len(), 2);
        assert!(config.get_claim_key(1).is_some());
        assert!(config.get_claim_key(2).is_some());

        assert_eq!(config.get_claim_key(1).unwrap(), [3u8; 32]);
    }

    #[test]
    fn test_config_with_claim_keys_from_toml() {
        let key1 = STANDARD.encode([3u8; 32]);

        let toml_content = format!(
            r#"
            listen_on_port = 8080
            internal_port = 8081
            permits = 10
            token_rate = 5.0
            workspace = "/tmp/test"
            storage_backend = "local"

            [claim_keys]
            1 = "{key1}"
            2 = "uBhfVeH0b7KQKfwOJqhwzLXKBpg7xLPBe5HjCksDDWg="
        "#
        );

        let config: Config = toml::from_str(&toml_content).unwrap();

        assert_eq!(config.listen_on_port, 8080);
        assert!(!config.claim_keys.is_empty());

        assert_eq!(config.claim_keys.len(), 2);
        assert!(config.get_claim_key(1).is_some());
        assert!(config.get_claim_key(2).is_some());

        assert_eq!(config.get_claim_key(1).unwrap(), [3u8; 32]);
    }

    #[test]
    fn test_config_without_claim_keys() {
        let toml_content = r#"
            listen_on_port = 8080
            internal_port = 8081
            permits = 10
            token_rate = 5.0
            workspace = "/tmp/test"
            storage_backend = "local"
        "#;

        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(config.claim_keys.is_empty());
    }

    #[test]
    fn test_parse_claim_key_valid() {
        let key1 = STANDARD.encode([3u8; 32]);
        let result = parse_claim_key(&format!("1:{key1}"));
        assert!(result.is_ok());
        let (kid, key) = result.unwrap();
        assert_eq!(kid, 1);
        assert_eq!(key, [3u8; 32]);
    }

    #[test]
    fn test_parse_claim_key_invalid_format() {
        let result = parse_claim_key("invalid_format");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid format"));
    }

    #[test]
    fn test_parse_claim_key_invalid_kid() {
        let result = parse_claim_key("256:IaNHoHtWetGMPkHj6Iy8MZe5L3KlH8F6j6nRvJpYQYU=");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid kid"));

        let result = parse_claim_key("abc:IaNHoHtWetGMPkHj6Iy8MZe5L3KlH8F6j6nRvJpYQYU=");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid kid"));
    }

    #[test]
    fn test_config_merge_with_claim_keys() {
        let mut file_config = Config::default();
        let claim_keys = vec![(1, [1u8; 32]), (2, [2u8; 32])];
        file_config.claim_keys = claim_keys.clone();

        let cli_config = Config {
            listen_on_port: 9000,
            ..Default::default()
        };

        let merged = cli_config.merge_with_file(file_config);

        assert_eq!(merged.listen_on_port, 9000); // CLI value takes precedence
        assert_eq!(merged.claim_keys, claim_keys); // File value used when CLI is None
    }

    #[test]
    fn test_config_default_has_no_claim_keys() {
        let config = Config::default();
        assert!(config.claim_keys.is_empty());
    }
}
