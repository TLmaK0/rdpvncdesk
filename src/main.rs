//! rdpvncdesk: a cross-platform RDP client built on IronRDP, winit and softbuffer.

mod app;
mod deeplink;
mod hosts;
mod keymap;
mod launcher;
mod logfile;
mod phase;
mod theme;

use anyhow::Context as _;
use clap::Parser;
use ironrdp::client::config::{ConfigBuilder, Destination};
use ironrdp::client::rdp::{RdpClient, RdpOutputEvent};
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use winit::event_loop::EventLoop;

/// Cross-platform RDP client. Run without arguments to open the connection manager.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Server to connect to, as host or host:port (default port 3389).
    /// If omitted, the connection manager window opens.
    destination: Option<String>,

    /// User name.
    #[arg(short, long, env = "RDPVNCDESK_USER")]
    username: Option<String>,

    /// Windows domain.
    #[arg(short, long, env = "RDPVNCDESK_DOMAIN")]
    domain: Option<String>,

    /// Password. If omitted, it is asked interactively (recommended: it keeps it out of shell history).
    #[arg(short, long, env = "RDPVNCDESK_PASSWORD", hide_env_values = true)]
    password: Option<String>,

    /// Remote desktop width in pixels.
    #[arg(long, default_value_t = 1280)]
    width: u16,

    /// Remote desktop height in pixels.
    #[arg(long, default_value_t = 800)]
    height: u16,

    /// Read the password from the first line of standard input.
    #[arg(long, conflicts_with = "password")]
    password_stdin: bool,

    /// Connect without Network Level Authentication (NLA): no credentials are sent up front and
    /// the server shows its own Windows sign-in screen. Only works if the server allows it.
    /// Less safe: the password is then typed into the remote screen.
    #[arg(long, conflicts_with_all = ["password", "password_stdin"])]
    no_nla: bool,

    /// Register this program as the system handler for rdp:// links.
    #[arg(long, exclusive = true)]
    register_url_handler: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Sessions (a destination that isn't an rdp:// link) also log to a file.
    let session_log = cli
        .destination
        .as_deref()
        .filter(|d| !deeplink::is_link(d))
        .and_then(logfile::create);
    {
        use tracing_subscriber::prelude::*;
        let log_filter = tracing_subscriber::EnvFilter::try_from_env("RDPVNCDESK_LOG")
            .unwrap_or_else(|_| "warn".into());
        let file_layer = session_log.as_ref().and_then(|(file, _)| {
            let file = file.try_clone().ok()?;
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_filter(tracing_subscriber::EnvFilter::new(
                        // ironrdp_async debug only names the connection steps. Don't raise
                        // ironrdp_connector: at debug it logs the Client Info, password included.
                        "info,rdpvncdesk=debug,ironrdp_async=debug",
                    )),
            )
        });
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(log_filter))
            .with(file_layer)
            // Always sees IronRDP's connection steps, whatever the log level.
            .with(phase::PhaseTracker.with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("ironrdp_async", tracing::Level::DEBUG),
            ))
            .init();
    }
    if let Some((_, path)) = &session_log {
        tracing::info!(version = env!("CARGO_PKG_VERSION"), log = %path.display(), "session log");
    }

    if cli.register_url_handler {
        return deeplink::register();
    }

    let Some(destination) = cli.destination.clone() else {
        return launcher::run(None);
    };
    // rdp://... links (Windows and Linux pass them as an argument) go through the launcher,
    // which decides whether stored credentials may be used.
    if deeplink::is_link(&destination) {
        let link = deeplink::parse(&destination)
            .with_context(|| format!("unsupported link: {destination}"))?;
        return launcher::run(Some(link));
    }

    let username = match cli.username {
        Some(u) => u,
        // Without NLA the user name is optional: it only pre-fills the server's sign-in screen.
        None if cli.no_nla => String::new(),
        None => prompt("User: ")?,
    };
    let password = match cli.password {
        Some(p) => p,
        None if cli.no_nla => String::new(),
        None if cli.password_stdin => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("reading password from stdin")?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
        None => rpassword::prompt_password("Password: ").context("reading password")?,
    };

    let mut builder = ConfigBuilder::new()
        .with_destination(Destination::new(destination.clone()).context("invalid destination")?)
        .with_username(username)
        .with_password(password)
        .with_desktop_width(cli.width)
        .with_desktop_height(cli.height)
        .with_client_build(client_build())
        // Same hard-coded value as FreeRDP and IronRDP's own viewer.
        .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
        .with_client_name(client_name())
        .with_platform(current_platform())
        .with_credssp(!cli.no_nla)
        // Bulk compression stays off until the vendored fix for issue #6 (ironrdp-client used to
        // drop the decompressor on Deactivation-Reactivation, which GNOME Remote Desktop does
        // right after connecting) is verified against such a server. RDPVNCDESK_COMPRESSION=1
        // turns it on for that test.
        .with_compression(std::env::var_os("RDPVNCDESK_COMPRESSION").is_some_and(|v| v == "1"));
    if let Some(domain) = cli.domain {
        builder = builder.with_domain(domain);
    }
    let config = builder.build().context("building RDP configuration")?;

    let event_loop = EventLoop::<RdpOutputEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let (output_sender, mut output_receiver) = tokio::sync::mpsc::channel(64);

    let client = RdpClient::new(config, output_sender);
    let input_sender = client.input_sender();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating tokio runtime")?;

    // Bridge RDP output events onto the winit event loop.
    let forwarder = rt.spawn(async move {
        while let Some(event) = output_receiver.recv().await {
            if proxy.send_event(event).is_err() {
                break;
            }
        }
    });

    // A server that stops answering mid-connection would otherwise leave a black window forever.
    std::thread::spawn(|| {
        std::thread::sleep(CONNECT_TIMEOUT);
        if !phase::is_connected() {
            let step = phase::last_state();
            let msg = if step == "Credssp" {
                "Connection timed out while signing in (NLA): the server stopped answering. \
                 Check user, password and domain"
                    .to_owned()
            } else {
                format!(
                    "Connection timed out: the server stopped answering during step \"{}\"",
                    if step.is_empty() { "start" } else { &step }
                )
            };
            tracing::error!(step, "{msg}");
            eprintln!("{msg}");
            std::process::exit(1);
        }
    });

    std::thread::spawn(move || {
        rt.block_on(client.run());
        let _ = rt.block_on(forwarder);
    });

    let mut app = app::App::new(
        &event_loop,
        input_sender,
        format!("rdpvncdesk - {destination}"),
        (cli.width, cli.height),
    )?;
    event_loop.run_app(&mut app)?;

    std::process::exit(app.exit_code());
}

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn prompt(label: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    print!("{label}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn client_build() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    major * 100 + minor * 10 + patch
}

/// NetBIOS-style client name (max 15 chars).
fn client_name() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rdpvncdesk".to_owned());
    name.chars().take(15).collect()
}

fn current_platform() -> MajorPlatformType {
    if cfg!(windows) {
        MajorPlatformType::WINDOWS
    } else if cfg!(target_os = "macos") {
        MajorPlatformType::MACINTOSH
    } else if cfg!(target_os = "linux") {
        MajorPlatformType::UNIX
    } else {
        MajorPlatformType::UNSPECIFIED
    }
}
