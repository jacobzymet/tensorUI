use std::{
    io::ErrorKind,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tensorui::{
    app::App,
    config::{self, Config},
    system::open_in_browser,
    web,
};
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "tensorui",
    version,
    about = "TensorMI Harness — a local, lightweight, open source LLM harness for humanity"
)]
struct Cli {
    /// Use a specific TOML configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    /// Loopback address for the TensorMI Harness web server (overrides config/env). Non-loopback addresses are refused.
    #[arg(long, value_name = "ADDR")]
    bind: Option<SocketAddr>,

    /// Open the UI in the default browser
    #[arg(long)]
    open: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;

    let bind = Config::resolve_ui_bind(cli.bind, &config)?;
    let url = config::public_ui_url(bind);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    let listener = match runtime.block_on(TcpListener::bind(bind)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            return greet_running_instance(&url, bind);
        }
        Err(error) => return Err(error).with_context(|| format!("could not bind {bind}")),
    };

    let mut app = App::new(config, config_path);
    app.set_listen_addr(bind);

    let shared = Arc::new(Mutex::new(app));
    let server = {
        let shared = Arc::clone(&shared);
        runtime.spawn(async move { web::serve(shared, listener).await })
    };

    println!("TensorMI Harness listening on {url}");
    println!("  Chat     {url}/");
    println!("  Settings {url}/settings");
    if cli.open {
        let _ = open_in_browser(&url);
    }
    let result = runtime.block_on(async { server.await? });
    if let Ok(mut app) = shared.lock() {
        app.shutdown();
    }
    result
}

const FOCUS_TIMEOUT: Duration = Duration::from_secs(5);

fn greet_running_instance(url: &str, bind: SocketAddr) -> Result<()> {
    match focus_running_instance(url) {
        Some(_) => println!("TensorMI Harness is already running on {url}"),
        None => bail!(
            "{bind} is already in use by another program — pass --bind to choose a different address"
        ),
    }
    Ok(())
}

fn focus_running_instance(url: &str) -> Option<()> {
    let client = tensorui::http::app_blocking_client(FOCUS_TIMEOUT);
    let response = client.post(format!("{url}/api/focus")).send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let info: serde_json::Value = response.json().ok()?;
    if info.get("app").and_then(|app| app.as_str()) != Some(web::INSTANCE_MARKER) {
        return None;
    }
    Some(())
}
