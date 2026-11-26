// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use meshkeep::clock::SystemClock;
use meshkeep::config::{Config, Overrides};
use tracing::info;

/// Always-on storing proxy and web client for MeshCore companion radios.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Config file (TOML).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Radio: host[:port] for WiFi, or a serial device such as /dev/ttyACM0.
    #[arg(long)]
    radio: Option<String>,
    /// Directory for the database.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// App listener as name=host:port; repeat for more. Replaces the file's listeners.
    #[arg(long)]
    listen: Vec<String>,
    /// Web interface address (host:port), or "off".
    #[arg(long)]
    web: Option<String>,
    /// Web password.
    #[arg(long, env = "MESHKEEP_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Turn off web sign-in (trusted networks only).
    #[arg(long)]
    no_auth: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MESHKEEP_LOG")
                .unwrap_or_else(|_| "meshkeep=info".into()),
        )
        .with_ansi(false)
        .with_target(false)
        .init();

    let a = Args::parse();
    let cfg = Config::load(
        a.config.as_deref(),
        Overrides {
            radio: a.radio,
            data_dir: a.data_dir,
            listen: a.listen,
            web: a.web,
            password: a.password,
            no_auth: a.no_auth,
        },
    )
    .and_then(|c| c.check().map(|_| c));
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) => {
            eprintln!("meshkeep: {e}");
            std::process::exit(2);
        }
    };

    let mut run = match meshkeep::start(cfg.clone(), Arc::new(SystemClock::new())).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("meshkeep: {e}");
            std::process::exit(1);
        }
    };
    for (l, addr) in cfg.listeners.iter().zip(&run.listeners) {
        info!("app listener {} on {addr}", l.name);
    }
    if let Some(w) = run.web {
        let auth = if cfg.auth { "" } else { ", sign-in off" };
        info!("web interface on http://{w}{auth}");
    }
    tokio::select! {
        _ = run.wait() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
