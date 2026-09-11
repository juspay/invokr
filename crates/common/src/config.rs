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

    /// Read an *optional* sensitive env var. `None` when absent or blank.
    ///
    /// Distinct from [`Self::read_or_default`]: for a credential, "absent" must
    /// stay distinguishable from "some fallback value", because the fallback
    /// would itself be an accepted secret.
    async fn read_opt(&self, name: &str) -> Option<String> {
        #[cfg(feature = "kms")]
        if let Some(ref client) = self.client {
            return crate::kms::decrypt_opt(client, name)
                .await
                .filter(|v| !v.trim().is_empty());
        }
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
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
    pub path_prefix: String,
    pub mode: ServerMode,
    pub dashboard_prefix: String,
    pub dashboard_dist_dir: String,
}

impl ServerEnv {
    async fn new() -> Result<Self, String> {
        let listen_addr = get_from_env_or_default("INVOKR_LISTEN_ADDR", "0.0.0.0:8080".to_string());
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

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Which authentication stack the API serves.
///
/// Deliberately has **no default**. Every other variable in this file falls back
/// to a default when unset, which for `INVOKR_WORKER_MAX_CONCURRENT` is a
/// performance surprise and for authentication would be a service that boots
/// wide open and says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// No authentication. Every request resolves to a fixed development
    /// identity. Local development only.
    Disabled,
    /// OpenID Connect for humans, plus any configured machine credentials.
    Oidc,
}

impl AuthMode {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "oidc" => Ok(Self::Oidc),
            other => Err(format!(
                "unknown INVOKR_AUTH_MODE {other:?}; expected `disabled` or `oidc`"
            )),
        }
    }
}

/// How the service identifies itself to the OpenID Provider.
///
/// The provider itself is discovered from `issuer_url`, so any OIDC-compliant
/// issuer works without a code change.
#[derive(Debug, Clone)]
pub struct OidcEnv {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Scheme and host only; the callback path is derived from the API prefix.
    pub redirect_host: String,
}

/// Everything the API needs to authenticate a request.
///
/// Constructed by the API binary alone — the worker and library mode never
/// serve HTTP, so requiring `INVOKR_AUTH_MODE` of them would break embedding.
#[derive(Debug, Clone)]
pub struct AuthEnv {
    pub mode: AuthMode,
    pub oidc: Option<OidcEnv>,
    /// Marks a bearer token as an API key rather than a JWT, e.g. `ivk`.
    pub api_token_prefix: Option<String>,
    /// JSON array of `{token, principal[, email]}`. Sensitive.
    pub static_tokens: Option<String>,
    /// The pre-OIDC shared key. Present means it is still accepted; **removing
    /// the variable is how the mechanism is decommissioned**, with no redeploy.
    pub legacy_api_key: Option<String>,
    /// `false` only for local development over plain HTTP, where a `Secure`
    /// cookie is never sent back.
    pub secure_cookies: bool,
}

impl AuthEnv {
    pub async fn from_env() -> anyhow::Result<Self> {
        let reader = sensitive_reader().await?;

        // No `get_from_env_or_default` here, on purpose.
        let mode = AuthMode::parse(&get_from_env_unsafe::<String>("INVOKR_AUTH_MODE").map_err(
            |_| {
                anyhow::anyhow!(
                    "INVOKR_AUTH_MODE is not set; expected `disabled` or `oidc`. \
                     It has no default because an unset auth mode would otherwise \
                     decide itself."
                )
            },
        )?)
        .map_err(|e| anyhow::anyhow!(e))?;

        let oidc = match mode {
            AuthMode::Disabled => None,
            AuthMode::Oidc => Some(OidcEnv {
                issuer_url: required("INVOKR_OIDC_ISSUER_URL")?,
                client_id: required("INVOKR_OIDC_CLIENT_ID")?,
                client_secret: reader.read_opt("INVOKR_OIDC_CLIENT_SECRET").await,
                redirect_host: required("INVOKR_OIDC_REDIRECT_HOST")?,
            }),
        };

        let api_token_prefix = non_empty(std::env::var("INVOKR_API_TOKEN_PREFIX").ok());
        let static_tokens = reader.read_opt("INVOKR_API_STATIC_TOKENS").await;

        // A prefix with no token list authenticates nothing, and a token list
        // with no prefix can never be presented. Either alone is a typo.
        if api_token_prefix.is_some() != static_tokens.is_some() {
            anyhow::bail!(
                "INVOKR_API_TOKEN_PREFIX and INVOKR_API_STATIC_TOKENS must be set \
                 together: a prefix with no tokens authenticates nothing, and \
                 tokens with no prefix can never be presented"
            );
        }

        Ok(Self {
            mode,
            oidc,
            api_token_prefix,
            static_tokens,
            // No default: an unset key means the legacy mechanism is off, never
            // that a well-known development key is in force.
            legacy_api_key: reader.read_opt("INVOKR_API_KEY").await,
            secure_cookies: get_from_env_or_default("INVOKR_AUTH_SECURE_COOKIES", true),
        })
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    non_empty(std::env::var(name).ok())
        .ok_or_else(|| anyhow::anyhow!("{name} is required when INVOKR_AUTH_MODE=oidc"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub db: DbEnv,
    pub server: ServerEnv,
    pub worker: WorkerEnv,
    pub crypto: CryptoEnv,
    pub metrics: MetricsEnv,
    pub reaper: ReaperEnv,
}

/// Builds the KMS-aware reader used for every sensitive variable.
///
/// Shared by [`AppConfig::from_env`] and [`AuthEnv::from_env`] so the two agree
/// on whether ciphertext is expected — a mismatch would decrypt half the
/// secrets and read the rest verbatim.
async fn sensitive_reader() -> anyhow::Result<SensitiveEnvReader> {
    let kms_enabled: bool = get_from_env_or_default("INVOKR_KMS_ENABLED", false);

    #[cfg(not(feature = "kms"))]
    if kms_enabled {
        anyhow::bail!("INVOKR_KMS_ENABLED=true but invokr was compiled without the 'kms' feature");
    }

    Ok(SensitiveEnvReader {
        #[cfg(feature = "kms")]
        client: if kms_enabled {
            tracing::info!("KMS decryption enabled, initializing AWS KMS client");
            Some(crate::kms::new_client().await)
        } else {
            None
        },
    })
}

impl AppConfig {
    pub async fn from_env() -> anyhow::Result<Self> {
        let reader = sensitive_reader().await?;

        let db = DbEnv::new(&reader).await.map_err(|e| anyhow::anyhow!(e))?;
        let server = ServerEnv::new().await.map_err(|e| anyhow::anyhow!(e))?;
        let worker = WorkerEnv::new();
        let crypto = CryptoEnv::new(&reader)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let metrics = MetricsEnv::new();
        let reaper = ReaperEnv::new().map_err(|e| anyhow::anyhow!(e))?;

        Ok(Self {
            db,
            server,
            worker,
            crypto,
            metrics,
            reaper,
        })
    }
}
