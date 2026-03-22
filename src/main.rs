use clap::{Parser, Subcommand};
use pechincha::{
    config::PechinchaConfig, display, models::SearchQuery, providers::ProviderId,
    search::SearchOrchestrator, SortOrder,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "pechincha",
    version,
    about = "Compare prices across Brazilian e-commerce platforms"
)]
struct Cli {
    /// Product search query
    query: Option<String>,

    /// Comma-separated platform filter (ml,ali,shopee,amazon,kabum,magalu)
    #[arg(short, long, value_delimiter = ',')]
    platforms: Vec<String>,

    /// Sort results by: total-cost, price, price-desc, rating, relevance
    #[arg(short, long, default_value = "total-cost")]
    sort: String,

    /// Max results per provider
    #[arg(short = 'n', long, default_value_t = 10)]
    limit: usize,

    /// Output as JSON
    #[arg(short, long)]
    json: bool,

    /// Minimum price filter (BRL)
    #[arg(long)]
    min_price: Option<f64>,

    /// Maximum price filter (BRL)
    #[arg(long)]
    max_price: Option<f64>,

    /// Show tax breakdown
    #[arg(long, default_value_t = true)]
    taxes: bool,

    /// Config file path
    #[arg(long)]
    config: Option<String>,

    /// Connect to your browser via CDP for Shopee/AliExpress
    /// Launch browser with: chromium --remote-debugging-port=9222
    #[arg(long)]
    cdp_port: Option<u16>,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List available providers and their status
    Providers,
    /// Log in to a provider (saves session cookies from your browser or curl)
    Login {
        /// Provider to log in to (ml, ali, shopee, amazon, amazon_us, kabum, magalu, olx)
        provider: String,

        /// Extract cookies from your browser (chrome, brave, firefox, safari, edge, chromium)
        #[arg(long, default_value = "chrome")]
        from_browser: String,

        /// Import cookies from a curl command string instead (from DevTools "Copy as cURL")
        #[arg(long)]
        import_curl: Option<String>,

        /// Import cookies from a file containing a curl command
        #[arg(long)]
        import_curl_file: Option<String>,

        /// Open a browser window for manual login instead of extracting
        #[arg(long)]
        interactive: bool,
    },
    /// Log out from a provider (deletes saved cookies)
    Logout {
        /// Provider to log out from, or "all" to clear all sessions
        provider: String,
    },
    /// Manage the browser daemon (for Shopee/AliExpress via CDP)
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start browser daemon (first time: visible for login; after: use --headless)
    Start {
        /// Run headless (no visible window). Use after logging in with visible mode first.
        #[arg(long)]
        headless: bool,
    },
    /// Stop the browser daemon
    Stop,
    /// Show daemon status
    Status,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show current configuration
    Show,
    /// Create default config file
    Init,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = match cli.verbose {
        0 => "pechincha=warn",
        1 => "pechincha=info",
        2 => "pechincha=debug",
        _ => "pechincha=trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(false)
        .init();

    let mut config = PechinchaConfig::load(cli.config.as_deref().map(std::path::Path::new))
        .map_err(|e| anyhow::anyhow!(e))?;

    // CLI --cdp-port overrides config; auto-detect daemon if running
    if let Some(port) = cli.cdp_port {
        config.general.cdp_port = Some(port);
    } else if config.general.cdp_port.is_none() {
        // Auto-detect: our daemon, OR user's personal browser with CDP
        if pechincha::daemon::is_running() || pechincha::daemon::is_cdp_available(9222) {
            config.general.cdp_port = Some(9222);
        }
    }

    match cli.command {
        Some(Commands::Config { action }) => match action {
            ConfigAction::Show => {
                println!("{}", toml::to_string_pretty(&config)?);
            }
            ConfigAction::Init => {
                let path = cli
                    .config
                    .as_deref()
                    .map(std::path::Path::new)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(pechincha::config::default_config_path);
                config.save(Some(&path)).map_err(|e| anyhow::anyhow!(e))?;
                println!("Config written to {}", path.display());
            }
        },
        Some(Commands::Login { provider, from_browser, import_curl, import_curl_file, interactive }) => {
            let id: ProviderId = provider
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;

            let cookies = if interactive {
                // Open browser for manual login
                pechincha::browser::login_interactive(id)
                    .map_err(|e| anyhow::anyhow!(e))?
            } else if let Some(ref curl) = import_curl {
                let c = pechincha::cookies::parse_curl_cookies(curl);
                if c.is_empty() {
                    anyhow::bail!("No cookies found in curl command.");
                }
                pechincha::cookies::save_cookies(id, &c).map_err(|e| anyhow::anyhow!(e))?;
                c
            } else if let Some(ref path) = import_curl_file {
                let curl = std::fs::read_to_string(path)?;
                let c = pechincha::cookies::parse_curl_cookies(&curl);
                if c.is_empty() {
                    anyhow::bail!("No cookies found in curl file.");
                }
                pechincha::cookies::save_cookies(id, &c).map_err(|e| anyhow::anyhow!(e))?;
                c
            } else {
                // Default: extract cookies from browser (like yt-dlp --cookies-from-browser)
                let domain = pechincha::scraping::provider_domain(id);
                println!("Extracting {} cookies from {}...", id, from_browser);
                let cookies = pechincha::cookies::extract_browser_cookies(id, &from_browser, domain)
                    .map_err(|e| anyhow::anyhow!(e))?;
                if cookies.is_empty() {
                    anyhow::bail!(
                        "No cookies found for {} in {}. Make sure you're logged in to {} in your {} browser.",
                        id, from_browser, domain, from_browser
                    );
                }
                pechincha::cookies::save_cookies(id, &cookies).map_err(|e| anyhow::anyhow!(e))?;
                cookies
            };

            println!("Saved {} cookies for {}.", cookies.len(), id);

            // Show session cookies
            let session: Vec<&str> = cookies.iter()
                .filter(|c| {
                    let n = c.name.to_lowercase();
                    n.contains("session") || n.contains("token") || n.contains("auth")
                        || n.contains("sid") || n.contains("spc_") || n.contains("at-main")
                })
                .map(|c| c.name.as_str())
                .collect();
            if !session.is_empty() {
                println!("Session cookies: {}", session.join(", "));
            }
        }
        Some(Commands::Logout { provider }) => {
            if provider == "all" {
                for id in ProviderId::all() {
                    pechincha::cookies::delete_cookies(*id)
                        .map_err(|e| anyhow::anyhow!(e))?;
                }
                println!("Cleared all saved sessions.");
            } else {
                let id: ProviderId = provider
                    .parse()
                    .map_err(|e: String| anyhow::anyhow!(e))?;
                pechincha::cookies::delete_cookies(id)
                    .map_err(|e| anyhow::anyhow!(e))?;
                println!("Logged out from {}.", id);
            }
        }
        Some(Commands::Daemon { action }) => match action {
            DaemonAction::Start { headless } => {
                pechincha::daemon::start(headless)
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
            DaemonAction::Stop => {
                pechincha::daemon::stop()
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
            DaemonAction::Status => {
                println!("Daemon: {}", pechincha::daemon::status());
            }
        }
        Some(Commands::Providers) => {
            println!("Available providers:");
            for id in ProviderId::all() {
                let logged_in = if pechincha::cookies::has_cookies(*id) {
                    " (logged in)"
                } else {
                    ""
                };
                let status = if provider_enabled(&config, *id) {
                    "enabled"
                } else {
                    "disabled"
                };
                println!("  {:<16} {}{}", id.to_string(), status, logged_in);
            }
        }
        None => {
            let query_str = cli
                .query
                .ok_or_else(|| anyhow::anyhow!("Missing search query. Usage: pechincha \"product name\""))?;

            let platforms: Vec<ProviderId> = cli
                .platforms
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect();

            let sort: SortOrder = cli.sort.parse().unwrap_or_default();

            let min_price = cli
                .min_price
                .and_then(|p| rust_decimal::Decimal::try_from(p).ok());
            let max_price = cli
                .max_price
                .and_then(|p| rust_decimal::Decimal::try_from(p).ok());

            let query = SearchQuery {
                query: query_str,
                max_results: cli.limit,
                min_price,
                max_price,
                condition: None,
                sort,
                platforms,
            };

            let orchestrator = SearchOrchestrator::from_config(&config);
            let results = orchestrator.search(&query).await;

            if cli.json {
                display::print_json(&results);
            } else {
                display::print_results(&results, cli.taxes);
            }
        }
    }

    Ok(())
}

fn provider_enabled(config: &PechinchaConfig, id: ProviderId) -> bool {
    match id {
        ProviderId::MercadoLivre => config.providers.mercadolivre.enabled,
        ProviderId::AliExpress => config.providers.aliexpress.enabled,
        ProviderId::Shopee => config.providers.shopee.enabled,
        ProviderId::Amazon => config.providers.amazon.enabled,
        ProviderId::AmazonUS => config.providers.amazon_us.enabled,
        ProviderId::Kabum => config.providers.kabum.enabled,
        ProviderId::MagazineLuiza => config.providers.magalu.enabled,
        ProviderId::Olx => config.providers.olx.enabled,
    }
}
