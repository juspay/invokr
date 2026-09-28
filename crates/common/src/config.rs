use crate::{
    env::{get_from_env_or_default, get_from_env_unsafe},
    models::pg_cron_expr::PgCronExpr,
    tenant::validate_table_prefix,
};

// ---------------------------------------------------------------------------
// Sensitive-env reader: transparently KMS-decrypts when the feature is active
// ---------------------------------------------------------------------------

struct SensitiveEnvReader {
    #[cfg(feature = "kms")]
    client: Option<aws_sdk_kms::Client>,
}

impl SensitiveEnvReader {
    /// Read a *required* sensitive env var (KMS-decrypted when enabled).
    async fn read(&self, name: &str) -> Result<String, String> {
        #[cfg(feature = "kms")]
        if let Some(ref client) = self.client {
            return Ok(crate::kms::decrypt(client, name).await);
        }
        get_from_env_unsafe(name)
    }

    /// Read an *optional* sensitive env var, falling back to `default`.
    async fn read_or_default(&self, name: &str, default: String) -> String {
        #[cfg(feature = "kms")]
        if let Some(ref client) = self.client {
            return crate::kms::decrypt_opt(client, name)
                .await
                .unwrap_or(default);
        }
        std::env::var(name).unwrap_or(default)
    }
}

// ---------------------------------------------------------------------------
// Structured env types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DbEnv {
    pub url: String,
    pub pool_size: u32,
    /// Prefix for all per-workspace Invokr tables. Default empty (tables stay `jobs`, etc.).
    /// Set to e.g. `sched` to get `sched_jobs`, `sched_executions`, etc.
    pub table_prefix: String,
}

impl DbEnv {
    async fn new(reader: &SensitiveEnvReader) -> Result<Self, String> {
        let url = reader.read("INVOKR_DATABASE_URL").await?;
        let pool_size = get_from_env_or_default("INVOKR_DB_POOL_SIZE", 50);
        let table_prefix = get_from_env_or_default("INVOKR_TABLE_PREFIX", String::new());
        if !validate_table_prefix(&table_prefix) {
            return Err(format!(
                "INVOKR_TABLE_PREFIX '{}' is invalid: only alphanumeric and underscore allowed",
                table_prefix
            ));
        }
        Ok(Self {
            url,
            pool_size,
            table_prefix,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMode {
    Api,
    Dashboard,
    Both,
}

impl ServerMode {
    fn from_env() -> Self {
        match get_from_env_or_default("INVOKR_MODE", "api".to_string())
            .to_lowercase()
            .as_str()
        {
            "dashboard" => Self::Dashboard,
            "both" => Self::Both,
            _ => Self::Api,
        }
    }
}

fn normalize_prefix(raw: String) -> String {
    let p = raw.trim_matches('/');
    if p.is_empty() {
        String::new()
    } else {
        format!("/{p}")
    }
}

#[derive(Debug, Clone)]
pub struct ServerEnv {
    pub listen_addr: String,
    pub api_key: String,
    pub path_prefix: String,
    pub mode: ServerMode,
    pub dashboard_prefix: String,
    pub dashboard_dist_dir: String,
}

impl ServerEnv {
    async fn new(reader: &SensitiveEnvReader) -> Result<Self, String> {
        let listen_addr = get_from_env_or_default("INVOKR_LISTEN_ADDR", "0.0.0.0:8080".to_string());
        let api_key = reader
            .read_or_default("INVOKR_API_KEY", "dev-api-key".to_string())
            .await;
        let path_prefix =
            normalize_prefix(get_from_env_or_default("INVOKR_PATH_PREFIX", String::new()));
        let mode = ServerMode::from_env();
        let dashboard_prefix = normalize_prefix(get_from_env_or_default(
            "INVOKR_DASHBOARD_PATH_PREFIX",
            String::new(),
        ));
        let dashboard_dist_dir =
            get_from_env_or_default("INVOKR_DASHBOARD_DIST_DIR", "./dashboard-dist".to_string());
        Ok(Self {
            listen_addr,
            api_key,
            path_prefix,
            mode,
            dashboard_prefix,
            dashboard_dist_dir,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorkerEnv {
    pub max_concurrent: usize,
    pub poll_interval_ms: u64,
    pub config_cache_ttl_sec: u64,
    pub secret_cache_ttl_sec: u64,
    pub shutdown_timeout_sec: u64,
}

impl WorkerEnv {
    fn new() -> Self {
        Self {
            max_concurrent: get_from_env_or_default("INVOKR_WORKER_MAX_CONCURRENT", 50),
            poll_interval_ms: get_from_env_or_default("INVOKR_WORKER_POLL_INTERVAL_MS", 200),
            config_cache_ttl_sec: get_from_env_or_default("INVOKR_CONFIG_CACHE_TTL_SEC", 60),
            secret_cache_ttl_sec: get_from_env_or_default("INVOKR_SECRET_CACHE_TTL_SEC", 300),
            shutdown_timeout_sec: get_from_env_or_default("INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC", 30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CryptoEnv {
    pub encryption_key: String,
}

impl CryptoEnv {
    async fn new(reader: &SensitiveEnvReader) -> Result<Self, String> {
        let encryption_key = reader
            .read_or_default("INVOKR_ENCRYPTION_KEY", "0".repeat(64))
            .await;
        Ok(Self { encryption_key })
    }
}

#[derive(Debug, Clone)]
pub struct MetricsEnv {
    pub port: u16,
}

impl MetricsEnv {
    fn new() -> Self {
        Self {
            port: get_from_env_or_default("INVOKR_METRICS_PORT", 9090),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerHealthEnv {
    /// How long `/ready`'s `SELECT 1` may take before it is reported as failed.
    pub db_probe_timeout_ms: u64,
    /// Lower bound on the poll-loop staleness threshold. The threshold itself is
    /// `max(this, 10 x INVOKR_WORKER_POLL_INTERVAL_MS)`.
    pub stale_after_floor_ms: u64,
    pub server_workers: usize,
}

impl WorkerHealthEnv {
    pub fn new() -> Self {
        Self {
            db_probe_timeout_ms: get_from_env_or_default("INVOKR_HEALTH_DB_PROBE_TIMEOUT_MS", 2000),
            stale_after_floor_ms: get_from_env_or_default(
                "INVOKR_HEALTH_STALE_AFTER_FLOOR_MS",
                5000,
            ),
            server_workers: get_from_env_or_default("INVOKR_HEALTH_SERVER_WORKERS", 1),
        }
    }
}

impl Default for WorkerHealthEnv {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ReaperEnv {
    /// pg_cron expression controlling how often invokr's own dogfooded reaper
    /// fires per workspace. Read at workspace creation, baked into the
    /// workspace's pg_cron entry; changing it after the fact only affects
    /// newly-created workspaces. Validated as a 5-field PgCronExpr at startup
    /// so a typo fails fast instead of breaking the first `POST /workspaces`.
    pub cron_expression: String,
}

impl ReaperEnv {
    fn new() -> Result<Self, String> {
        let cron_expression =
            get_from_env_or_default("INVOKR_REAPER_CRON_EXPRESSION", "*/15 * * * *".to_string());
        PgCronExpr::try_from(cron_expression.clone()).map_err(|e| {
            format!(
                "Invalid INVOKR_REAPER_CRON_EXPRESSION '{}': {}",
                cron_expression, e
            )
        })?;
        Ok(Self { cron_expression })
    }
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub db: DbEnv,
    pub server: ServerEnv,
    pub worker: WorkerEnv,
    pub crypto: CryptoEnv,
    pub metrics: MetricsEnv,
    pub health: WorkerHealthEnv,
    pub reaper: ReaperEnv,
}

impl AppConfig {
    pub async fn from_env() -> anyhow::Result<Self> {
        let kms_enabled: bool = get_from_env_or_default("INVOKR_KMS_ENABLED", false);

        #[cfg(not(feature = "kms"))]
        if kms_enabled {
            anyhow::bail!(
                "INVOKR_KMS_ENABLED=true but invokr was compiled without the 'kms' feature"
            );
        }

        let reader = SensitiveEnvReader {
            #[cfg(feature = "kms")]
            client: if kms_enabled {
                tracing::info!("KMS decryption enabled, initializing AWS KMS client");
                Some(crate::kms::new_client().await)
            } else {
                None
            },
        };

        let db = DbEnv::new(&reader).await.map_err(|e| anyhow::anyhow!(e))?;
        let server = ServerEnv::new(&reader)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let worker = WorkerEnv::new();
        let crypto = CryptoEnv::new(&reader)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let metrics = MetricsEnv::new();
        let health = WorkerHealthEnv::new();
        let reaper = ReaperEnv::new().map_err(|e| anyhow::anyhow!(e))?;

        Ok(Self {
            db,
            server,
            worker,
            crypto,
            metrics,
            health,
            reaper,
        })
    }
}
