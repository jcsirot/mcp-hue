//! Runtime configuration, read from environment variables.

use std::fmt;

/// Address of the Hue bridge (`host` or `host:port`).
pub const BRIDGE_HOST_ENV: &str = "HUE_BRIDGE_HOST";
/// Hue application key used to authenticate against the bridge.
pub const APP_KEY_ENV: &str = "HUE_APP_KEY";
/// Expected bridge identifier, checked against the bridge certificate.
pub const BRIDGE_ID_ENV: &str = "HUE_BRIDGE_ID";

/// A Hue application key. It is a secret: `Debug` never prints its value.
#[derive(Clone, PartialEq, Eq)]
pub struct AppKey(String);

impl AppKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw key, for use in the `hue-application-key` request header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AppKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppKey(***)")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub bridge_host: Option<String>,
    pub app_key: Option<AppKey>,
    pub bridge_id: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds the configuration from any variable source. Blank values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            bridge_host: get(BRIDGE_HOST_ENV),
            app_key: get(APP_KEY_ENV).map(AppKey::new),
            bridge_id: get(BRIDGE_ID_ENV),
        }
    }
}

/// Loads the optional `.env` file; real environment variables take precedence.
/// Returns a warning to log when the file exists but could not be loaded entirely.
pub fn load_dotenv() -> Option<String> {
    dotenvy::dotenv().err().and_then(|err| dotenv_warning(&err))
}

fn dotenv_warning(err: &dotenvy::Error) -> Option<String> {
    if err.not_found() {
        return None;
    }
    Some(match err {
        // The parse error quotes the faulty line, which may hold the application
        // key: never forward it.
        dotenvy::Error::LineParse(..) => "the .env file has a syntax error and was only \
            partially loaded (line not shown: it may hold a secret)"
            .to_owned(),
        dotenvy::Error::Io(io) => format!("the .env file could not be read: {io}"),
        _ => "the .env file could not be loaded".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_syntax_error_warning_hides_the_line() {
        let err = dotenvy::Error::LineParse("HUE_APP_KEY=\"secret-key".to_owned(), 12);
        assert!(err.to_string().contains("secret-key"), "premise changed");

        let warning = dotenv_warning(&err).unwrap();
        assert!(!warning.contains("secret-key"), "key leaked in: {warning}");
        assert!(warning.contains("syntax error"));
    }

    #[test]
    fn missing_dotenv_file_is_silent() {
        let err = dotenvy::Error::Io(std::io::ErrorKind::NotFound.into());
        assert_eq!(dotenv_warning(&err), None);
    }

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn reads_all_variables() {
        let config = Config::from_lookup(lookup(&[
            (BRIDGE_HOST_ENV, "192.168.1.7"),
            (APP_KEY_ENV, "secret-key"),
            (BRIDGE_ID_ENV, "ecb5fafffe13af9c"),
        ]));
        assert_eq!(config.bridge_host.as_deref(), Some("192.168.1.7"));
        assert_eq!(
            config.app_key.as_ref().map(AppKey::expose),
            Some("secret-key")
        );
        assert_eq!(config.bridge_id.as_deref(), Some("ecb5fafffe13af9c"));
    }

    #[test]
    fn missing_and_blank_values_are_unset() {
        let config = Config::from_lookup(lookup(&[(BRIDGE_HOST_ENV, "  "), (APP_KEY_ENV, "")]));
        assert_eq!(config, Config::default());
    }

    #[test]
    fn values_are_trimmed() {
        let config = Config::from_lookup(lookup(&[(APP_KEY_ENV, " secret-key\r\n")]));
        assert_eq!(
            config.app_key.as_ref().map(AppKey::expose),
            Some("secret-key")
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let config = Config::from_lookup(lookup(&[(APP_KEY_ENV, "secret-key")]));
        let debug = format!("{config:?} {:?}", config.app_key);
        assert!(!debug.contains("secret-key"), "key leaked in: {debug}");
        assert!(debug.contains("***"));
    }
}
