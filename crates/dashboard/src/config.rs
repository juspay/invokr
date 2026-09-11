use serde::{Deserialize, Serialize};

/// What the dashboard needs to reach the API.
///
/// Deliberately carries **no credential**. This struct is serialised into the
/// SSR'd HTML, so anything on it is readable by anyone who can load the page —
/// which is exactly how the service-wide API key used to leak. The browser
/// authenticates with its `HttpOnly` session cookie, which script cannot read
/// and this struct must never carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    pub api_base_url: String,
    pub api_prefix: String,
    pub dashboard_prefix: String,
}

impl DashboardConfig {
    pub fn api_base(&self) -> String {
        if self.api_base_url.is_empty() {
            self.api_prefix.clone()
        } else {
            format!(
                "{}{}",
                self.api_base_url.trim_end_matches('/'),
                self.api_prefix
            )
        }
    }
}
