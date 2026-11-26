// SPDX-License-Identifier: AGPL-3.0-or-later
//! Configuration: a TOML file, overridable by command line flags.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub enum RadioAddr {
    Tcp(String),
    Serial(String),
}

impl RadioAddr {
    /// `/dev/...` or `COMn` is a serial port, anything else is `host:port`.
    pub fn parse(s: &str) -> Result<RadioAddr, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("radio address is empty".into());
        }
        if s.starts_with('/') || s.to_ascii_uppercase().starts_with("COM") {
            return Ok(RadioAddr::Serial(s.to_string()));
        }
        if !s.contains(':') {
            return Ok(RadioAddr::Tcp(format!("{s}:5000")));
        }
        Ok(RadioAddr::Tcp(s.to_string()))
    }
}

impl std::fmt::Display for RadioAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RadioAddr::Tcp(a) => write!(f, "{a}"),
            RadioAddr::Serial(p) => write!(f, "{p}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Listener {
    pub name: String,
    pub bind: SocketAddr,
    /// Track sync position per remote IP address instead of per listener.
    pub per_address: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub radio: Option<RadioAddr>,
    pub data_dir: PathBuf,
    pub listeners: Vec<Listener>,
    pub web_bind: Option<SocketAddr>,
    pub password: Option<String>,
    pub auth: bool,
    /// Show messages sent from one client (web or app) in the other apps.
    pub mirror_sent: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    radio: Option<String>,
    data_dir: Option<PathBuf>,
    mirror_sent: Option<bool>,
    #[serde(default)]
    listener: Vec<FileListener>,
    web: Option<FileWeb>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileListener {
    name: String,
    bind: String,
    #[serde(default)]
    per_address: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWeb {
    bind: Option<String>,
    password: Option<String>,
    auth: Option<bool>,
}

/// Command line overrides.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub radio: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub listen: Vec<String>,
    pub web: Option<String>,
    pub password: Option<String>,
    pub no_auth: bool,
}

fn parse_bind(s: &str) -> Result<SocketAddr, String> {
    let s = s.trim();
    let full = if s.starts_with(':') {
        format!("0.0.0.0{s}")
    } else if !s.contains(':') {
        format!("0.0.0.0:{s}")
    } else {
        s.to_string()
    };
    full.parse()
        .map_err(|_| format!("bad listen address '{s}', use host:port"))
}

/// `name=bind` from the command line.
fn parse_listen_flag(s: &str) -> Result<Listener, String> {
    let (name, bind) = s
        .split_once('=')
        .ok_or_else(|| format!("bad --listen '{s}', use name=host:port"))?;
    Ok(Listener {
        name: name.trim().to_string(),
        bind: parse_bind(bind)?,
        per_address: false,
    })
}

impl Config {
    pub fn load(path: Option<&Path>, o: Overrides) -> Result<Config, String> {
        let file: FileConfig = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                toml::from_str(&text)
                    .map_err(|e| format!("{}: {}", p.display(), e.to_string().replace('\n', " ")))?
            }
            None => FileConfig::default(),
        };
        let web = file.web.unwrap_or_default();

        let radio = match o.radio.or(file.radio) {
            Some(r) if !r.trim().is_empty() => Some(RadioAddr::parse(&r)?),
            _ => None,
        };

        let mut listeners = Vec::new();
        if o.listen.is_empty() {
            for l in file.listener {
                listeners.push(Listener {
                    name: l.name,
                    bind: parse_bind(&l.bind)?,
                    per_address: l.per_address,
                });
            }
        } else {
            for l in &o.listen {
                listeners.push(parse_listen_flag(l)?);
            }
        }
        if listeners.is_empty() {
            listeners.push(Listener {
                name: "app".into(),
                bind: "0.0.0.0:5000".parse().unwrap(),
                per_address: false,
            });
        }
        let mut seen = std::collections::HashSet::new();
        for l in &listeners {
            if l.name.is_empty() || l.name.contains('@') {
                return Err(format!(
                    "listener name '{}' must be non-empty and contain no '@'",
                    l.name
                ));
            }
            if !seen.insert(l.name.clone()) {
                return Err(format!("listener name '{}' is used twice", l.name));
            }
        }

        let web_bind = match o.web.or(web.bind) {
            Some(s) if s.trim().eq_ignore_ascii_case("off") => None,
            Some(s) => Some(parse_bind(&s)?),
            None => Some("0.0.0.0:8080".parse().unwrap()),
        };
        let auth = !o.no_auth && web.auth.unwrap_or(true);
        let password = o.password.or(web.password).filter(|p| !p.is_empty());

        Ok(Config {
            radio,
            data_dir: o
                .data_dir
                .or(file.data_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            listeners,
            web_bind,
            password,
            auth,
            mirror_sent: file.mirror_sent.unwrap_or(true),
        })
    }

    /// Problems that stop meshkeep from starting, as one-line messages.
    pub fn check(&self) -> Result<(), String> {
        if self.radio.is_none() {
            return Err("no radio configured: set radio in the config file or pass --radio".into());
        }
        if self.web_bind.is_some() && self.auth && self.password.is_none() {
            return Err(
                "web password not set: set [web] password, or auth = false for a trusted network"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_addr_forms() {
        assert_eq!(
            RadioAddr::parse("/dev/ttyACM0").unwrap(),
            RadioAddr::Serial("/dev/ttyACM0".into())
        );
        assert_eq!(
            RadioAddr::parse("10.0.0.5").unwrap(),
            RadioAddr::Tcp("10.0.0.5:5000".into())
        );
        assert_eq!(
            RadioAddr::parse("radio.lan:5001").unwrap(),
            RadioAddr::Tcp("radio.lan:5001".into())
        );
    }

    #[test]
    fn file_and_flags() {
        let dir = std::env::temp_dir().join(format!("meshkeep-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.toml");
        std::fs::write(
            &p,
            r#"
radio = "/dev/ttyUSB0"
mirror_sent = false
data_dir = "/var/lib/meshkeep"
[[listener]]
name = "phone"
bind = "0.0.0.0:5000"
[[listener]]
name = "tablet"
bind = ":5001"
per_address = true
[web]
bind = "127.0.0.1:8081"
password = "pw"
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p), Overrides::default()).unwrap();
        assert_eq!(c.listeners.len(), 2);
        assert!(c.listeners[1].per_address);
        assert!(!c.mirror_sent);
        assert_eq!(c.listeners[1].bind.port(), 5001);
        assert!(c.check().is_ok());
        let c = Config::load(
            Some(&p),
            Overrides {
                radio: Some("1.2.3.4:5000".into()),
                no_auth: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c.radio, Some(RadioAddr::Tcp("1.2.3.4:5000".into())));
        assert!(!c.auth);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn password_required_with_auth() {
        let c = Config::load(
            None,
            Overrides {
                radio: Some("x:1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(c.check().unwrap_err().contains("password"));
        assert_eq!(c.listeners[0].bind.port(), 5000);
        assert!(c.mirror_sent);
    }
}
