//! The dashboard's API client.
//!
//! Every request authenticates with the browser's `HttpOnly` session cookie,
//! never a token this code can read. The API and dashboard are served from the
//! same origin, so `SameOrigin` credentials are sent on each call and no
//! `Authorization` header is built here at all.
//!
//! This replaced a `Bearer` header carrying the service-wide `INVOKR_API_KEY`,
//! which the server rendered into the page for this code to read back — meaning
//! anyone who could load the dashboard held full API access to every workspace.

use super::models::*;

#[cfg(feature = "hydrate")]
use gloo_net::http::Request;
#[cfg(feature = "hydrate")]
use web_sys::RequestCredentials;

#[cfg(feature = "hydrate")]
use crate::config::DashboardConfig;

#[cfg(feature = "hydrate")]
fn get_config() -> DashboardConfig {
    use wasm_bindgen::JsValue;
    let window = web_sys::window().expect("no global window");
    let config = js_sys::Reflect::get(&window, &JsValue::from_str("__INVOKR_CONFIG__"))
        .unwrap_or(JsValue::UNDEFINED);
    let get = |key: &str| -> String {
        if config.is_undefined() || config.is_null() {
            return String::new();
        }
        js_sys::Reflect::get(&config, &JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    };
    DashboardConfig {
        api_base_url: get("apiBaseUrl"),
        api_prefix: get("apiPrefix"),
        dashboard_prefix: get("dashboardPrefix"),
    }
}

// ============================================================================
// Hydrate (WASM) implementations — actual HTTP calls via gloo-net
// ============================================================================

#[cfg(feature = "hydrate")]
mod inner {
    use super::*;

    pub async fn list_organizations() -> Result<Vec<Organization>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/orgs"))
            .credentials(RequestCredentials::SameOrigin)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Vec<Organization>> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn get_organization(org_id: String) -> Result<Organization, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/orgs/{org_id}"))
            .credentials(RequestCredentials::SameOrigin)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Organization> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_organization(body: CreateOrganization) -> Result<Organization, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/orgs"))
            .credentials(RequestCredentials::SameOrigin)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Organization> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn update_organization(
        org_id: String,
        body: UpdateOrganization,
    ) -> Result<Organization, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::put(&format!("{base}/v1/orgs/{org_id}"))
            .credentials(RequestCredentials::SameOrigin)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Organization> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    // -- Workspace API --

    pub async fn list_workspaces(org_id: String) -> Result<Vec<Workspace>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/orgs/{org_id}/workspaces"))
            .credentials(RequestCredentials::SameOrigin)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Vec<Workspace>> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_workspace(
        org_id: String,
        body: CreateWorkspace,
    ) -> Result<Workspace, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/orgs/{org_id}/workspaces"))
            .credentials(RequestCredentials::SameOrigin)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Workspace> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    // -- Jobs API (workspace-scoped) --

    /// Percent-encodes a query value (RFC 3986: unreserved pass through, else `%XX`).
    fn encode_query_value(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    fn push_query_param(qs: &mut String, key: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        qs.push(if qs.is_empty() { '?' } else { '&' });
        qs.push_str(key);
        qs.push('=');
        qs.push_str(&encode_query_value(value));
    }

    pub async fn list_jobs(
        org_id: String,
        workspace_id: String,
        params: JobListQueryParams,
    ) -> Result<PaginatedResponse<Job>, String> {
        let config = get_config();
        let base = config.api_base();
        let mut qs = String::new();
        if let Some(cursor) = &params.cursor {
            push_query_param(&mut qs, "cursor", cursor);
        }
        push_query_param(&mut qs, "limit", &params.limit.to_string());
        if let Some(job_id) = &params.job_id {
            push_query_param(&mut qs, "job_id", job_id);
        }
        if !params.status.is_empty() {
            push_query_param(&mut qs, "status", &params.status.join(","));
        }
        if !params.trigger.is_empty() {
            push_query_param(&mut qs, "trigger_type", &params.trigger.join(","));
        }
        if !params.endpoint_type.is_empty() {
            push_query_param(&mut qs, "endpoint_type", &params.endpoint_type.join(","));
        }
        if let Some(endpoint) = &params.endpoint {
            push_query_param(&mut qs, "endpoint", endpoint);
        }
        if let Some(after) = &params.created_after {
            push_query_param(&mut qs, "created_after", after);
        }
        if let Some(before) = &params.created_before {
            push_query_param(&mut qs, "created_before", before);
        }
        let resp = Request::get(&format!("{base}/v1/jobs{qs}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<Job> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data)
    }

    pub async fn get_job(
        org_id: String,
        workspace_id: String,
        job_id: String,
    ) -> Result<Job, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/jobs/{job_id}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Job> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_job(
        org_id: String,
        workspace_id: String,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/jobs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn cancel_job(
        org_id: String,
        workspace_id: String,
        job_id: String,
    ) -> Result<serde_json::Value, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/jobs/{job_id}/cancel"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data)
    }

    pub async fn get_job_status(
        org_id: String,
        workspace_id: String,
        job_id: String,
    ) -> Result<JobStatus, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/jobs/{job_id}/status"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<JobStatus> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn get_job_versions(
        org_id: String,
        workspace_id: String,
        job_id: String,
    ) -> Result<Vec<Job>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/jobs/{job_id}/versions"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Vec<Job>> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    // -- Endpoints API (workspace-scoped) --

    pub async fn list_endpoints(
        org_id: String,
        workspace_id: String,
    ) -> Result<Vec<Endpoint>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/endpoints"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<Endpoint> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_endpoint(
        org_id: String,
        workspace_id: String,
        body: CreateEndpoint,
    ) -> Result<Endpoint, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/endpoints"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Endpoint> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn update_endpoint(
        org_id: String,
        workspace_id: String,
        name: String,
        body: serde_json::Value,
    ) -> Result<Endpoint, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::put(&format!("{base}/v1/endpoints/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Endpoint> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn delete_endpoint(
        org_id: String,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::delete(&format!("{base}/v1/endpoints/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    // -- Executions API (workspace-scoped) --

    pub async fn list_job_executions(
        org_id: String,
        workspace_id: String,
        job_id: String,
    ) -> Result<Vec<Execution>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/jobs/{job_id}/executions"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<Execution> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn get_execution(
        org_id: String,
        workspace_id: String,
        execution_id: String,
    ) -> Result<Execution, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/executions/{execution_id}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Execution> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn cancel_execution(
        org_id: String,
        workspace_id: String,
        execution_id: String,
    ) -> Result<serde_json::Value, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/executions/{execution_id}/cancel"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data)
    }

    pub async fn list_attempts(
        org_id: String,
        workspace_id: String,
        execution_id: String,
    ) -> Result<Vec<Attempt>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/executions/{execution_id}/attempts"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Vec<Attempt>> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn list_execution_logs(
        org_id: String,
        workspace_id: String,
        execution_id: String,
    ) -> Result<Vec<ExecutionLog>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/executions/{execution_id}/logs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Vec<ExecutionLog>> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    // -- Configs API (workspace-scoped) --

    pub async fn list_configs(org_id: String, workspace_id: String) -> Result<Vec<Config>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/configs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<Config> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_config(
        org_id: String,
        workspace_id: String,
        body: CreateConfig,
    ) -> Result<Config, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/configs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Config> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn update_config(
        org_id: String,
        workspace_id: String,
        name: String,
        body: UpdateConfig,
    ) -> Result<Config, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::put(&format!("{base}/v1/configs/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Config> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn delete_config(
        org_id: String,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::delete(&format!("{base}/v1/configs/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    // -- Payload Specs API (workspace-scoped) --

    pub async fn list_payload_specs(
        org_id: String,
        workspace_id: String,
    ) -> Result<Vec<PayloadSpec>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/payload-specs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<PayloadSpec> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_payload_spec(
        org_id: String,
        workspace_id: String,
        body: CreatePayloadSpec,
    ) -> Result<PayloadSpec, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/payload-specs"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<PayloadSpec> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn update_payload_spec(
        org_id: String,
        workspace_id: String,
        name: String,
        body: UpdatePayloadSpec,
    ) -> Result<PayloadSpec, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::put(&format!("{base}/v1/payload-specs/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<PayloadSpec> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn delete_payload_spec(
        org_id: String,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::delete(&format!("{base}/v1/payload-specs/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    // -- Secrets API (workspace-scoped) --

    pub async fn list_secrets(org_id: String, workspace_id: String) -> Result<Vec<Secret>, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::get(&format!("{base}/v1/secrets"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: PaginatedResponse<Secret> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn create_secret(
        org_id: String,
        workspace_id: String,
        body: CreateSecret,
    ) -> Result<Secret, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::post(&format!("{base}/v1/secrets"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Secret> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn update_secret(
        org_id: String,
        workspace_id: String,
        name: String,
        body: UpdateSecret,
    ) -> Result<Secret, String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::put(&format!("{base}/v1/secrets/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .json(&body)
            .map_err(|e| e.to_string())?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: DataResponse<Secret> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data.data)
    }

    pub async fn delete_secret(
        org_id: String,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        let config = get_config();
        let base = config.api_base();
        let resp = Request::delete(&format!("{base}/v1/secrets/{name}"))
            .credentials(RequestCredentials::SameOrigin)
            .header("X-Org-Id", &org_id)
            .header("X-Workspace-Id", &workspace_id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.ok() {
            return Err(format!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }
}

// ============================================================================
// SSR stubs — these exist so the code compiles for the server target.
// They are never called (LocalResource only fetches on the client).
// ============================================================================

#[cfg(feature = "ssr")]
mod inner {
    use super::*;

    pub async fn list_organizations() -> Result<Vec<Organization>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn get_organization(_org_id: String) -> Result<Organization, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_organization(_body: CreateOrganization) -> Result<Organization, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn update_organization(
        _org_id: String,
        _body: UpdateOrganization,
    ) -> Result<Organization, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_workspaces(_org_id: String) -> Result<Vec<Workspace>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_workspace(
        _org_id: String,
        _body: CreateWorkspace,
    ) -> Result<Workspace, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_jobs(
        _org_id: String,
        _workspace_id: String,
        _params: JobListQueryParams,
    ) -> Result<PaginatedResponse<Job>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn get_job(
        _org_id: String,
        _workspace_id: String,
        _job_id: String,
    ) -> Result<Job, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_job(
        _org_id: String,
        _workspace_id: String,
        _body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn cancel_job(
        _org_id: String,
        _workspace_id: String,
        _job_id: String,
    ) -> Result<serde_json::Value, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn get_job_status(
        _org_id: String,
        _workspace_id: String,
        _job_id: String,
    ) -> Result<JobStatus, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn get_job_versions(
        _org_id: String,
        _workspace_id: String,
        _job_id: String,
    ) -> Result<Vec<Job>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_endpoints(
        _org_id: String,
        _workspace_id: String,
    ) -> Result<Vec<Endpoint>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_endpoint(
        _org_id: String,
        _workspace_id: String,
        _body: CreateEndpoint,
    ) -> Result<Endpoint, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn update_endpoint(
        _org_id: String,
        _workspace_id: String,
        _name: String,
        _body: serde_json::Value,
    ) -> Result<Endpoint, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn delete_endpoint(
        _org_id: String,
        _workspace_id: String,
        _name: String,
    ) -> Result<(), String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_job_executions(
        _org_id: String,
        _workspace_id: String,
        _job_id: String,
    ) -> Result<Vec<Execution>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn get_execution(
        _org_id: String,
        _workspace_id: String,
        _execution_id: String,
    ) -> Result<Execution, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn cancel_execution(
        _org_id: String,
        _workspace_id: String,
        _execution_id: String,
    ) -> Result<serde_json::Value, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_attempts(
        _org_id: String,
        _workspace_id: String,
        _execution_id: String,
    ) -> Result<Vec<Attempt>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_execution_logs(
        _org_id: String,
        _workspace_id: String,
        _execution_id: String,
    ) -> Result<Vec<ExecutionLog>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_configs(
        _org_id: String,
        _workspace_id: String,
    ) -> Result<Vec<Config>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_config(
        _org_id: String,
        _workspace_id: String,
        _body: CreateConfig,
    ) -> Result<Config, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn update_config(
        _org_id: String,
        _workspace_id: String,
        _name: String,
        _body: UpdateConfig,
    ) -> Result<Config, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn delete_config(
        _org_id: String,
        _workspace_id: String,
        _name: String,
    ) -> Result<(), String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_payload_specs(
        _org_id: String,
        _workspace_id: String,
    ) -> Result<Vec<PayloadSpec>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_payload_spec(
        _org_id: String,
        _workspace_id: String,
        _body: CreatePayloadSpec,
    ) -> Result<PayloadSpec, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn update_payload_spec(
        _org_id: String,
        _workspace_id: String,
        _name: String,
        _body: UpdatePayloadSpec,
    ) -> Result<PayloadSpec, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn delete_payload_spec(
        _org_id: String,
        _workspace_id: String,
        _name: String,
    ) -> Result<(), String> {
        Err("SSR: not available".to_string())
    }
    pub async fn list_secrets(
        _org_id: String,
        _workspace_id: String,
    ) -> Result<Vec<Secret>, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn create_secret(
        _org_id: String,
        _workspace_id: String,
        _body: CreateSecret,
    ) -> Result<Secret, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn update_secret(
        _org_id: String,
        _workspace_id: String,
        _name: String,
        _body: UpdateSecret,
    ) -> Result<Secret, String> {
        Err("SSR: not available".to_string())
    }
    pub async fn delete_secret(
        _org_id: String,
        _workspace_id: String,
        _name: String,
    ) -> Result<(), String> {
        Err("SSR: not available".to_string())
    }
}

// Re-export from the active inner module
pub use inner::*;
