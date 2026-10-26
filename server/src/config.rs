mod settings;
pub use settings::*;

use std::{env, num::ParseIntError, path::PathBuf, str::FromStr};

use thiserror::Error;
use url::Url;

use crate::tdx::{TdxCredentials, TdxCredentialsError};

pub fn load_dotenv() {
    dotenvy::dotenv().ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")).ok();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Development,
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
            "production" => Ok(Self::Production),
            _ => Err(ConfigError::InvalidEnvironment),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("configuration {path}: {source}")]
    File {
        path: PathBuf,
        #[source]
        source: Box<ConfigError>,
    },
    #[error("Invalid configuration: {0}")]
    Invalid(String),
    #[error("VIA_ENV must be explicitly set to development or production")]
    MissingEnvironment,
    #[error("VIA_ENV must be development or production")]
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
        let mut settings = ViaConfig::load(None)?;
        settings.apply_environment()?;
        Self::from_settings(&settings)
    }

    pub fn from_settings(settings: &ViaConfig) -> Result<Self, ConfigError> {
        let environment = Environment::required_from_env()?;
        settings.validate_environment(environment)?;
        let static_path = env::var_os("VIA_STATIC")
            .map(PathBuf::from)
            .or_else(|| settings.server.static_path.clone())
            .ok_or(ConfigError::MissingStaticPath)?;
        if static_path.as_os_str().is_empty() {
            return Err(ConfigError::MissingStaticPath);
        }
        Ok(Self {
            environment,
            static_path,
            port: settings.server.port,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TdxRuntimeConfig {
    pub api_base_url: Url,
    pub credentials: TdxCredentials,
}

impl TdxRuntimeConfig {
    pub fn from_settings(settings: &TdxSettings) -> Result<Self, ConfigError> {
        Ok(Self {
            api_base_url: parse_url("tdx.api_base_url", &settings.api_base_url)?,
            credentials: TdxCredentials::from_client_credentials(
                required_credential("TDX_CLIENT_ID")?,
                required_credential("TDX_CLIENT_SECRET")?,
            ),
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let mut settings = ViaConfig::load(None)?;
        settings.apply_environment()?;
        Self::from_settings(&settings.tdx)
    }
}

#[derive(Clone, Debug)]
pub struct ValhallaRuntimeConfig {
    pub base_url: Url,
}

impl ValhallaRuntimeConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut settings = ViaConfig::load(None)?;
        settings.apply_environment()?;
        Ok(Self {
            base_url: parse_url("valhalla.base_url", &settings.valhalla.base_url)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub(crate) environment: Environment,
    pub(crate) valhalla: ValhallaRuntimeConfig,
    pub(crate) tdx: TdxRuntimeConfig,
    pub(crate) settings: ViaConfig,
}

impl RuntimeConfig {
    pub fn from_settings(
        environment: Environment,
        settings: ViaConfig,
    ) -> Result<Self, ConfigError> {
        settings.validate_environment(environment)?;
        let tdx = TdxRuntimeConfig::from_settings(&settings.tdx)?;
        let valhalla = ValhallaRuntimeConfig {
            base_url: parse_url("valhalla.base_url", &settings.valhalla.base_url)?,
        };
        Ok(Self {
            environment,
            tdx,
            valhalla,
            settings,
        })
    }

    pub fn from_env(environment: Environment) -> Result<Self, ConfigError> {
        let mut settings = ViaConfig::load(None)?;
        settings.apply_environment()?;
        Self::from_settings(environment, settings)
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
        None => Ok(ViaConfig::default().server.port),
    }
}

#[cfg(test)]
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
        ConfigError, Environment, TdxRuntimeConfig, ValhallaRuntimeConfig, parse_port,
        validate_transport_security,
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
            "production".parse::<Environment>().unwrap(),
            Environment::Production
        );
        assert!(matches!(
            "staging".parse::<Environment>(),
            Err(ConfigError::InvalidEnvironment)
        ));
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
        assert!(validate_transport_security(Environment::Development, &valhalla, &tdx).is_ok());
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
            api_base_url: Url::parse(&super::ViaConfig::default().tdx.api_base_url).unwrap(),
            credentials,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("private-id"));
        assert!(!debug.contains("private-secret"));
    }
}
