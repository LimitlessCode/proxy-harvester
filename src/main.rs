mod geo;
mod pool;
mod scraper;
mod types;
mod validator;

use clap::Parser;
use pool::ProxyPool;
use scraper::all_scrapers;
use std::path::PathBuf;
use validator::{Validator, ValidatorConfig};

/// High-quality proxy harvester — elite-only, with multi-stage validation.
///
/// Scrapes public proxy lists, validates every proxy through
/// connectivity, anonymity, header-leak, and consistency checks.
/// Only proxies passing ALL gates are kept.
#[derive(Parser, Debug)]
#[command(name = "proxy-harvester", version, about)]
struct Cli {
    /// Max concurrent proxy checks
    #[arg(long, default_value = "100")]
    concurrency: usize,

    /// Consistency checks per proxy (must pass all)
    #[arg(long, default_value = "3")]
    checks: u8,

    /// Max acceptable latency in ms
    #[arg(long, default_value = "5000")]
    max_latency: u64,

    /// Output file for verified proxies (JSON)
    #[arg(short, long, default_value = "elite-proxies.json")]
    output: PathBuf,

    /// Continuously re-harvest every N seconds (0 = run once)
    #[arg(short, long, default_value = "0")]
    interval: u64,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Quick connectivity pre-filter before full validation
    #[arg(long)]
    quick_filter: bool,

    /// Only keep proxies from this country (ISO 2-letter code, e.g. US, GB, DE)
    #[arg(long)]
    country: Option<String>,

    /// Reject proxies from these countries (comma-separated ISO codes, e.g. CN,RU)
    #[arg(long, value_delimiter = ',')]
    exclude_country: Vec<String>,

    /// Max proxies to validate (limits how many raw proxies enter validation)
    #[arg(long, default_value = "500")]
    limit: usize,

    /// Minimum anonymity level: elite, anonymous, or transparent
    #[arg(long, default_value = "elite")]
    anonymity: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();

    // Setup logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_target(false)
        .init();

    let min_anonymity = match cli.anonymity.to_lowercase().as_str() {
        "elite" => types::AnonymityLevel::Elite,
        "anonymous" => types::AnonymityLevel::Anonymous,
        "transparent" => types::AnonymityLevel::Transparent,
        other => {
            eprintln!("Invalid anonymity level: {other}. Use: elite, anonymous, transparent");
            std::process::exit(1);
        }
    };

    let config = ValidatorConfig {
        concurrency: cli.concurrency,
        consistency_checks: cli.checks,
        max_latency_ms: cli.max_latency,
        allowed_countries: cli.country.iter().cloned().collect(),
        excluded_countries: cli.exclude_country.clone(),
        min_anonymity,
        ..Default::default()
    };

    let validator = Validator::new(config.clone()).await?;
    let pool = ProxyPool::new();

    if cli.interval > 0 {
        tracing::info!(
            interval_secs = cli.interval,
            "Starting continuous harvest mode"
        );
        loop {
            run_harvest(&validator, &pool, &config, &cli, cli.quick_filter).await;
            tracing::info!("Sleeping {}s until next harvest...", cli.interval);
            tokio::time::sleep(std::time::Duration::from_secs(cli.interval)).await;
        }
    } else {
        run_harvest(&validator, &pool, &config, &cli, cli.quick_filter).await;
    }

    Ok(())
}

async fn run_harvest(
    validator: &Validator,
    pool: &ProxyPool,
    config: &ValidatorConfig,
    cli: &Cli,
    quick_filter: bool,
) {
    // ── Phase 1: Scrape all sources ──
    tracing::info!("Scraping proxy sources...");
    let scrapers = all_scrapers();
    let mut raw = Vec::new();

    for scraper in &scrapers {
        match scraper.harvest().await {
            Ok(proxies) => {
                tracing::info!(
                    source = scraper.name(),
                    count = proxies.len(),
                    "Harvested"
                );
                raw.extend(proxies);
            }
            Err(e) => {
                tracing::warn!(
                    source = scraper.name(),
                    error = %e,
                    "Scraper failed, continuing"
                );
            }
        }
    }

    if raw.is_empty() {
        tracing::error!("No proxies harvested from any source");
        pool::print_stats(&crate::types::HarvestStats {
            raw_collected: 0,
            after_dedup: 0,
            connectivity_passed: 0,
            anonymity_passed: 0,
            elite_confirmed: 0,
            duration_secs: 0.0,
            sources_scraped: scrapers.len(),
        });
        return;
    }

    // ── Phase 2: Truncate to limit ──
    if raw.len() > cli.limit {
        tracing::info!(
            total = raw.len(),
            limit = cli.limit,
            "Truncating proxy list"
        );
        raw.truncate(cli.limit);
    }

    // ── Phase 3 (optional): Quick connectivity pre-filter ──
    let to_validate = if quick_filter {
        tracing::info!(count = raw.len(), "Running quick connectivity filter...");
        let alive = Validator::quick_filter(config, raw).await;
        tracing::info!(alive = alive.len(), "Quick filter complete");
        alive
    } else {
        raw
    };

    // ── Phase 3: Full validation ──
    tracing::info!(count = to_validate.len(), "Starting full validation...");
    let (verified, stats) = validator.validate(to_validate).await;

    pool::print_stats(&stats);

    // ── Phase 4: Store results ──
    if !verified.is_empty() {
        // Save to pool
        pool.merge(verified.clone()).await;

        // Write to output file
        let json = serde_json::to_string_pretty(&verified).unwrap_or_else(|_| "[]".to_string());
        if let Err(e) = std::fs::write(&cli.output, &json) {
            tracing::error!(path = %cli.output.display(), error = %e, "Failed to write output");
        } else {
            tracing::info!(
                path = %cli.output.display(),
                count = verified.len(),
                "Elite proxies written"
            );
        }

        // Print a few for quick use
        println!("\n┌── Sample Elite Proxies ──────────────────────────────┐");
        for (i, p) in verified.iter().take(5).enumerate() {
            let flag = p.country.as_deref().unwrap_or("??");
            let loc = p.city.as_deref().unwrap_or("");
            println!(
                "│ {:>2}. {:>21}:{:<5} [{:<5}] {:>4}ms {:>2} {:<15} │",
                i + 1,
                p.ip,
                p.port,
                p.protocol.to_string(),
                p.avg_latency_ms,
                flag,
                loc,
            );
        }
        println!("└──────────────────────────────────────────────────────┘");
    }
}
