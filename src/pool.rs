use crate::types::{HarvestStats, VerifiedProxy};
use chrono::Utc;
use rand::seq::SliceRandom;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Thread-safe pool of verified proxies with rotation support
pub struct ProxyPool {
    proxies: Arc<RwLock<Vec<VerifiedProxy>>>,
}

impl ProxyPool {
    pub fn new() -> Self {
        Self {
            proxies: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Replace the entire pool with freshly validated proxies
    pub async fn refresh(&self, fresh: Vec<VerifiedProxy>) {
        let mut pool = self.proxies.write().await;
        *pool = fresh;
    }

    /// Merge in new proxies, keeping existing ones that haven't expired
    pub async fn merge(&self, fresh: Vec<VerifiedProxy>) {
        let mut pool = self.proxies.write().await;
        let now = Utc::now();

        // Keep existing non-expired
        pool.retain(|p| p.expires_at > now);

        // Add fresh, deduping by external_ip
        for f in fresh {
            if !pool.iter().any(|p| p.external_ip == f.external_ip) {
                pool.push(f);
            }
        }

        tracing::info!(pool_size = pool.len(), "Pool merged");
    }

    /// Get a random elite proxy
    pub async fn random(&self) -> Option<VerifiedProxy> {
        let pool = self.proxies.read().await;
        let now = Utc::now();
        let live: Vec<_> = pool.iter().filter(|p| p.expires_at > now).cloned().collect();
        live.choose(&mut rand::thread_rng()).cloned()
    }

    /// Get N random proxies
    pub async fn random_n(&self, n: usize) -> Vec<VerifiedProxy> {
        let pool = self.proxies.read().await;
        let now = Utc::now();
        let mut live: Vec<_> = pool.iter().filter(|p| p.expires_at > now).cloned().collect();
        let mut rng = rand::thread_rng();
        live.shuffle(&mut rng);
        live.truncate(n);
        live
    }

    /// Get all live proxies
    pub async fn all_live(&self) -> Vec<VerifiedProxy> {
        let pool = self.proxies.read().await;
        let now = Utc::now();
        pool.iter().filter(|p| p.expires_at > now).cloned().collect()
    }

    /// Current pool size (live + expired)
    pub async fn total(&self) -> usize {
        self.proxies.read().await.len()
    }

    /// Live proxy count
    pub async fn live_count(&self) -> usize {
        self.all_live().await.len()
    }

    /// Export pool to JSON
    pub async fn export_json(&self) -> String {
        let pool = self.proxies.read().await;
        serde_json::to_string_pretty(&*pool).unwrap_or_else(|_| "[]".to_string())
    }

    /// Remove expired entries
    pub async fn purge_expired(&self) -> usize {
        let mut pool = self.proxies.write().await;
        let before = pool.len();
        let now = Utc::now();
        pool.retain(|p| p.expires_at > now);
        before - pool.len()
    }
}

/// Print a summary of a harvest run
pub fn print_stats(stats: &HarvestStats) {
    println!("╔══════════════════════════════════════╗");
    println!("║       HARVEST RUN COMPLETE          ║");
    println!("╠══════════════════════════════════════╣");
    println!("║ Raw collected:    {:>6}             ║", stats.raw_collected);
    println!("║ After dedup:      {:>6}             ║", stats.after_dedup);
    println!("║ Elite confirmed:  {:>6}             ║", stats.elite_confirmed);
    println!("║ Sources scraped:  {:>6}             ║", stats.sources_scraped);
    println!("║ Duration:         {:>6.1}s          ║", stats.duration_secs);
    println!("╚══════════════════════════════════════╝");

    if stats.elite_confirmed == 0 {
        println!("\n⚠️  No elite proxies found this run.");
    } else {
        println!(
            "\n✅ {} elite proxies ready. {}% pass rate from raw.",
            stats.elite_confirmed,
            if stats.raw_collected > 0 {
                (stats.elite_confirmed as f64 / stats.raw_collected as f64 * 100.0) as u32
            } else {
                0
            }
        );
    }
}
