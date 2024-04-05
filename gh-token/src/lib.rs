#![doc(html_root_url = "https://docs.rs/gh-token/0.1.1")]
#![allow(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::uninlined_format_args
)]

use crate::error::ParseError;
use serde_derive::Deserialize;
use std::env;
use std::fmt::{self, Debug, Display};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
struct Config {
    #[serde(rename = "github.com")]
    github_com: Option<Host>,
}

#[derive(Deserialize)]
struct Host {
    oauth_token: Option<String>,
}

pub enum Error {
    NotConfigured(PathBuf),
    Parse(error::ParseError),
}

mod error {
    use std::io;
    use std::path::PathBuf;

    pub enum ParseError {
        EnvNonUtf8(&'static str),
        Io(PathBuf, io::Error),
        Yaml(PathBuf, serde_yaml::Error),
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NotConfigured(path) => {
                write!(
                    formatter,
                    "no github.com token found in {}; use `gh auth login` to authenticate",
                    path.display(),
                )
            }
            Error::Parse(ParseError::EnvNonUtf8(var)) => {
                write!(
                    formatter,
                    "environment variable ${} contains non-utf8 value",
                    var,
                )
            }
            Error::Parse(ParseError::Io(path, io_error)) => {
                write!(formatter, "failed to read {}: {}", path.display(), io_error)
            }
            Error::Parse(ParseError::Yaml(path, yaml_error)) => {
                write!(
                    formatter,
                    "failed to parse {}: {}",
                    path.display(),
                    yaml_error,
                )
            }
        }
    }
}

impl Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "Error({:?})", self.to_string())
    }
}

pub fn get() -> Result<String, Error> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(token_from_env) = env::var_os(var) {
            return token_from_env
                .into_string()
                .map_err(|_| Error::Parse(ParseError::EnvNonUtf8(var)));
        }
    }

    let Some(path) = hosts_config_file() else {
        let fallback_path = Path::new("~").join(".config").join("gh").join("hosts.yml");
        return Err(Error::NotConfigured(fallback_path));
    };

    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(io_error) => {
            return Err(if io_error.kind() == ErrorKind::NotFound {
                Error::NotConfigured(path)
            } else {
                Error::Parse(ParseError::Io(path, io_error))
            });
        }
    };

    let config: Config = match serde_yaml::from_slice(&content) {
        Ok(config) => config,
        Err(yaml_error) => return Err(Error::Parse(ParseError::Yaml(path, yaml_error))),
    };

    if let Some(github_com) = config.github_com {
        if let Some(oauth_token) = github_com.oauth_token {
            return Ok(oauth_token);
        }
    }

    // While support for `gh auth token` is being rolled out, do not report
    // errors from it yet. It probably means the user's installed `gh` does not
    // have the feature.
    //
    // "As of right now storing the authentication token in the system keyring
    // is an opt-in feature, but in the near future it will be required"
    if let Some(token) = token_from_cli() {
        return Ok(token);
    }

    // When system keyring auth tokens become required in the near future, this
    // message needs to change to stop recommending putting a plain-text token
    // into that yaml file.
    Err(Error::NotConfigured(path))
}

fn hosts_config_file() -> Option<PathBuf> {
    let config_dir = config_dir()?;
    Some(config_dir.join("hosts.yml"))
}

fn config_dir() -> Option<PathBuf> {
    if let Some(gh_config_dir) = env::var_os("GH_CONFIG_DIR") {
        if !gh_config_dir.is_empty() {
            return Some(PathBuf::from(gh_config_dir));
        }
    }

    if let Some(xdg_config_home) = env::var_os("XDG_CONFIG_HOME") {
        if !xdg_config_home.is_empty() {
            return Some(Path::new(&xdg_config_home).join("gh"));
        }
    }

    if cfg!(windows) {
        if let Some(app_data) = env::var_os("AppData") {
            if !app_data.is_empty() {
                return Some(Path::new(&app_data).join("GitHub CLI"));
            }
        }
    }

    let home_dir = home::home_dir()?;
    Some(home_dir.join(".config").join("gh"))
}

fn token_from_cli() -> Option<String> {
    let output = Command::new("gh").arg("auth").arg("token").output().ok()?;
    let mut token = String::from_utf8(output.stdout).ok()?;
    // Trim the captured trailing newline from CLI output
    let token_len = token.trim_end().len();
    token.truncate(token_len);
    Some(token)
}
