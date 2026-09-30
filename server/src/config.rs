use std::{env, num::ParseIntError, path::PathBuf, str::FromStr};

use thiserror::Error;
use url::Url;

use crate::providers::valhalla::DEFAULT_VALHALLA_BASE_URL;
use crate::tdx::{DEFAULT_TDX_API_BASE_URL, TdxCredentials, TdxCredentialsError};

const DEFAULT_PORT: u16 = 3000;

pub fn load_dotenv() {
    dotenvy::dotenv().ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")).ok();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Development,
    Staging,
    Production,
}

impl Environment {
    pub fn required_from_env() -> Result<Self, ConfigError> {
        let value = env::var("VIA_ENV").map_err(|_| ConfigError::MissingEnvironment)?;
        Self::from_str(&value)
    }

    /// Matches the live-routing CLI's existing default when VIA_ENV is absent.
    pub fn from_env_or_development() -> Result<Self, ConfigError> {
        match env::var("VIA_ENV") {
            Ok(value) => Self::from_str(&value),
            Err(env::VarError::NotPresent) => Ok(Self::Development),
            Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidEnvironment),
        }
    }

    pub fn is_production(self) -> bool {
        self == Self::Production
    }
}

impl FromStr for Environment {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "development" => Ok(Self::Development),
            "staging" => Ok(Self::Staging),
            "production" => Ok(Self::Production),
            _ => Err(ConfigError::InvalidEnvironment),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("VIA_ENV must be explicitly set to development, staging, or production")]
    MissingEnvironment,
    #[error("VIA_ENV must be development, staging, or production")]
    InvalidEnvironment,
    #[error("VIA_STATIC must point to a validated .via-static.json artifact")]
    MissingStaticPath,
    #[error("PORT is not a valid TCP port: {0}")]
    InvalidPort(#[source] ParseIntError),
    #[error("{setting} is not a valid URL: {source}")]
    InvalidUrl {
        setting: &'static str,
        #[source]
        source: url::ParseError,
    },
    #[error("{setting} must use HTTPS in production")]
    InsecureProductionUrl { setting: &'static str },
    #[error(transparent)]
    TdxCredentials(#[from] TdxCredentialsError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub environment: Environment,
    pub static_path: PathBuf,
    pub port: u16,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let environment = Environment::required_from_env()?;
        let static_path = env::var("VIA_STATIC").map_err(|_| ConfigError::MissingStaticPath)?;
        let port = match env::var("PORT") {
            Ok(value) => parse_port(Some(&value))?,
            Err(_) => parse_port(None)?,
        };
        Ok(Self {
            environment,
            static_path: PathBuf::from(static_path),
            port,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TdxRuntimeConfig {
    pub api_base_url: Url,
    pub credentials: TdxCredentials,
}

impl TdxRuntimeConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_base_url = match env::var("VIA_TDX_BASE_URL") {
            Ok(value) => parse_url("VIA_TDX_BASE_URL", &value)?,
            Err(_) => Url::parse(DEFAULT_TDX_API_BASE_URL).expect("static TDX API URL is valid"),
        };
        let credentials = TdxCredentials::from_client_credentials(
            required_credential("TDX_CLIENT_ID")?,
            required_credential("TDX_CLIENT_SECRET")?,
        );
        Ok(Self {
            api_base_url,
            credentials,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ValhallaRuntimeConfig {
    pub base_url: Url,
}

impl ValhallaRuntimeConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let base_url = match env::var("VIA_VALHALLA_URL") {
            Ok(value) => parse_url("VIA_VALHALLA_URL", &value)?,
            Err(_) => Url::parse(DEFAULT_VALHALLA_BASE_URL).expect("static Valhalla URL is valid"),
        };
        Ok(Self { base_url })
    }
}

#[derive(Clone, Debug)]
pub struct ProductionRuntimeConfig {
    pub(crate) valhalla: ValhallaRuntimeConfig,
    pub(crate) tdx: TdxRuntimeConfig,
}

impl ProductionRuntimeConfig {
    pub fn from_env(environment: Environment) -> Result<Self, ConfigError> {
        let valhalla = ValhallaRuntimeConfig::from_env()?;
        let tdx = TdxRuntimeConfig::from_env()?;
        validate_transport_security(environment, &valhalla, &tdx)?;
        Ok(Self { valhalla, tdx })
    }
}

fn required_credential(name: &'static str) -> Result<String, TdxCredentialsError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(TdxCredentialsError::Missing(name)),
    }
}

fn parse_url(setting: &'static str, value: &str) -> Result<Url, ConfigError> {
    Url::parse(value).map_err(|source| ConfigError::InvalidUrl { setting, source })
}

fn parse_port(value: Option<&str>) -> Result<u16, ConfigError> {
    match value {
        Some(value) => value.parse().map_err(ConfigError::InvalidPort),
        None => Ok(DEFAULT_PORT),
    }
}

fn validate_transport_security(
    environment: Environment,
    valhalla: &ValhallaRuntimeConfig,
    tdx: &TdxRuntimeConfig,
) -> Result<(), ConfigError> {
    if !environment.is_production() {
        return Ok(());
    }
    if valhalla.base_url.scheme() != "https" {
        return Err(ConfigError::InsecureProductionUrl {
            setting: "VIA_VALHALLA_URL",
        });
    }
    if tdx.api_base_url.scheme() != "https" {
        return Err(ConfigError::InsecureProductionUrl {
            setting: "VIA_TDX_BASE_URL",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigError, DEFAULT_TDX_API_BASE_URL, Environment, TdxRuntimeConfig,
        ValhallaRuntimeConfig, parse_port, validate_transport_security,
    };
    use crate::tdx::TdxCredentials;
    use url::Url;

    #[test]
    fn environment_accepts_only_supported_values() {
        assert_eq!(
            "development".parse::<Environment>().unwrap(),
            Environment::Development
        );
        assert_eq!(
            "staging".parse::<Environment>().unwrap(),
            Environment::Staging
        );
        assert_eq!(
            "production".parse::<Environment>().unwrap(),
            Environment::Production
        );
        assert!(matches!(
            "prod".parse::<Environment>(),
            Err(ConfigError::InvalidEnvironment)
        ));
    }

    #[test]
    fn port_defaults_and_validates_as_a_tcp_port() {
        assert_eq!(parse_port(None).unwrap(), 3000);
        assert_eq!(parse_port(Some("8080")).unwrap(), 8080);
        assert!(matches!(
            parse_port(Some("65536")),
            Err(ConfigError::InvalidPort(_))
        ));
    }

    #[test]
    fn production_requires_https_for_both_upstream_urls() {
        let credentials = TdxCredentials::from_client_credentials("client", "secret");
        let valhalla = ValhallaRuntimeConfig {
            base_url: Url::parse("http://valhalla:8002/").unwrap(),
        };
        let tdx = TdxRuntimeConfig {
            api_base_url: Url::parse("https://tdx.example/api/").unwrap(),
            credentials,
        };
        assert!(matches!(
            validate_transport_security(Environment::Production, &valhalla, &tdx),
            Err(ConfigError::InsecureProductionUrl {
                setting: "VIA_VALHALLA_URL"
            })
        ));
        let secure_valhalla = ValhallaRuntimeConfig {
            base_url: Url::parse("https://valhalla.example/").unwrap(),
        };
        let insecure_tdx = TdxRuntimeConfig {
            api_base_url: Url::parse("http://tdx.example/api/").unwrap(),
            credentials: TdxCredentials::from_client_credentials("client", "secret"),
        };
        assert!(matches!(
            validate_transport_security(Environment::Production, &secure_valhalla, &insecure_tdx),
            Err(ConfigError::InsecureProductionUrl {
                setting: "VIA_TDX_BASE_URL"
            })
        ));
        assert!(validate_transport_security(Environment::Staging, &valhalla, &tdx).is_ok());
        assert!(
            validate_transport_security(
                Environment::Production,
                &secure_valhalla,
                &TdxRuntimeConfig {
                    api_base_url: Url::parse("https://tdx.example/api/").unwrap(),
                    credentials: TdxCredentials::from_client_credentials("client", "secret"),
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn tdx_credentials_debug_output_is_redacted() {
        let credentials = TdxCredentials::from_client_credentials("private-id", "private-secret");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("private-id"));
        assert!(!debug.contains("private-secret"));
        assert!(debug.contains("<redacted>"));

        let config = TdxRuntimeConfig {
            api_base_url: Url::parse(DEFAULT_TDX_API_BASE_URL).unwrap(),
            credentials,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("private-id"));
        assert!(!debug.contains("private-secret"));
    }
}
