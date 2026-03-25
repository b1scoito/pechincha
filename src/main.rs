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

    /// Output as CSV
    #[arg(long)]
    csv: bool,

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

    // CLI --cdp-port overrides config; auto-detect if port 9222 is listening
    if let Some(port) = cli.cdp_port {
        config.general.cdp_port = Some(port);
    } else if config.general.cdp_port.is_none() {
        if std::net::TcpStream::connect("127.0.0.1:9222").is_ok() {
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
        Some(Commands::Providers) => {
            println!("Available providers:");
            for id in ProviderId::all() {
                let status = if provider_enabled(&config, *id) {
                    "enabled"
                } else {
                    "disabled"
                };
                println!("  {:<16} {}", id.to_string(), status);
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

            if cli.csv {
                display::print_csv(&results);
            } else if cli.json {
                display::print_json(&results);
            } else {
                display::print_results(&results, &query.query);
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
