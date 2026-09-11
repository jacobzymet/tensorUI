use std::{
    io::ErrorKind,
    net::SocketAddr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tensor::{
    app::App,
    config::{self, Config},
    desktop, system, web,
};
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "tensor",
    version,
    about = "Tensor — a local, lightweight, open source LLM harness for humanity"
)]
struct Cli {
    /// Use a specific TOML configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    /// Loopback address for the Tensor web server (overrides config/env). Non-loopback addresses are refused.
    #[arg(long, value_name = "ADDR")]
    bind: Option<SocketAddr>,

    /// Run the local server without opening the desktop window (or a browser).
    #[arg(long)]
    headless: bool,

    /// Open the UI in the default browser instead of the desktop window.
    #[arg(long)]
    browser: bool,

    /// Deprecated alias for `--browser`.
    #[arg(long, hide = true)]
    open: bool,

    /// Internal: retry the loopback bind after a self-update restart.
    #[arg(long, hide = true)]
    update_restart: bool,
}

fn main() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("could not install the rustls Ring crypto provider"))?;

    let cli = Cli::parse();
    let open_browser = cli.browser || cli.open;
    tensor::updates::cleanup_previous_install();
    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;

    let bind = Config::resolve_ui_bind(cli.bind, &config)?;
    let url = config::public_ui_url(bind);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    let (bind_attempts, bind_delay) = tensor::updates::bind_retry_budget(cli.update_restart);
    let mut listener = None;
    for attempt in 0..bind_attempts {
        match runtime.block_on(TcpListener::bind(bind)) {
            Ok(bound) => {
                listener = Some(bound);
                break;
            }
            Err(error) if error.kind() == ErrorKind::AddrInUse && cli.update_restart => {
                if attempt + 1 < bind_attempts {
                    thread::sleep(bind_delay);
                }
            }
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                return greet_running_instance(bind);
            }
            Err(error) => return Err(error).with_context(|| format!("could not bind {bind}")),
        }
    }
    let listener = match listener {
        Some(listener) => listener,
        None => return greet_running_instance(bind),
    };

    let mut app = App::new(config, config_path).map_err(anyhow::Error::msg)?;
    app.set_listen_addr(bind);

    let shared = Arc::new(Mutex::new(app));
    let mut server = {
        let shared = Arc::clone(&shared);
        runtime.spawn(async move { web::serve(shared, listener).await })
    };

    println!("Tensor listening on {url}");
    println!("  Chat     {url}/");
    println!("  Settings {url}/settings");

    let result = if cli.headless || open_browser {
        if open_browser {
            let _ = system::open_in_browser(&url);
        }
        runtime.block_on(async {
            tokio::select! {
                result = &mut server => result?,
                result = tokio::signal::ctrl_c() => {
                    server.abort();
                    result.map_err(Into::into)
                }
                _ = tensor::updates::wait_for_restart_request() => {
                    server.abort();
                    Ok(())
                }
            }
        })
    } else {
        // Native desktop window on the main thread; server keeps running on Tokio.
        let window_result = desktop::run_window(&url, bind, runtime.handle());
        server.abort();
        match runtime.block_on(server) {
            Ok(Ok(())) | Err(_) => {}
            Ok(Err(error)) => return Err(error),
        }
        window_result
    };

    runtime.block_on(web::shutdown_private_work());
    if let Ok(mut app) = shared.lock() {
        app.shutdown();
    }
    tensor::updates::spawn_restart_if_pending();
    result
}

const FOCUS_TIMEOUT: Duration = Duration::from_secs(5);

fn greet_running_instance(bind: SocketAddr) -> Result<()> {
    match focus_running_instance(bind) {
        Some(_) => println!("Tensor is already running — focusing the open window."),
        None => bail!(
            "{bind} is already in use by another program — pass --bind to choose a different address"
        ),
    }
    Ok(())
}

fn focus_running_instance(bind: SocketAddr) -> Option<()> {
    let url = config::loopback_ui_url(bind);
    let client = tensor::http::app_blocking_client(FOCUS_TIMEOUT);
    // Bootstrap the per-process HttpOnly session cookie exactly as a browser
    // navigation does before calling the authenticated local API.
    // Probe via the bind address — Chrome maps *.localhost itself; reqwest uses OS DNS.
    client.get(format!("{url}/")).send().ok()?;
    let response = client.post(format!("{url}/api/focus")).send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = tensor::http::blocking_response_bytes_limited(response, 64 * 1024).ok()?;
    let info: serde_json::Value = serde_json::from_slice(&body).ok()?;
    if !matches!(
        info.get("app").and_then(|app| app.as_str()),
        Some(web::INSTANCE_MARKER | "tensorui")
    ) {
        return None;
    }
    Some(())
}
