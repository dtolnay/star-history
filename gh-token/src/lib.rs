#![doc(html_root_url = "https://docs.rs/gh-token/0.1.0")]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

use crate::error::ParseError;
use serde_derive::Deserialize;
use std::env;
use std::fmt::{self, Debug, Display};
use std::fs;
use std::path::{Path, PathBuf};

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
    let Some(path) = hosts_config_file() else {
        let fallback_path = Path::new("~").join(".config").join("gh").join("hosts.yml");
        return Err(Error::NotConfigured(fallback_path));
    };

    match path.try_exists() {
        Ok(true) => {}
        Ok(false) => return Err(Error::NotConfigured(path)),
        Err(io_error) => return Err(Error::Parse(ParseError::Io(path, io_error))),
    }

    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(io_error) => return Err(Error::Parse(ParseError::Io(path, io_error))),
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
