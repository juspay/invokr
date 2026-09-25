use actix_web::{dev::Payload, web, Error, FromRequest, HttpMessage, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use invokr_common::{
    db,
    error::AppError,
    models::{
        endpoint::EndpointType,
        job::{JobStatus, TriggerType},
    },
    tenant::WorkspaceContext,
};
use std::{
    future::{self, Future},
    pin::Pin,
};

use crate::{auth::Caller, router::AppState};

/// Proof that the request carried a valid credential, and who presented it.
///
/// The middleware does the authenticating; this only reads the [`Caller`] it
/// resolved. Keeping it as an extractor means a handler that forgets it is a
/// handler that does not compile against an authenticated route — the same
/// ergonomics as before, now carrying an identity rather than asserting a
/// string comparison succeeded.
///
/// **This is not authorization.** Invokr has no policy layer: any authenticated
/// caller reaches every org and workspace.
//
// The caller is carried rather than discarded so that audit logging, and any
// future authorization check, have an identity to work with. Handlers take it
// as `_auth` today and read nothing from it, hence the allow.
pub struct AuthenticatedRequest(#[allow(dead_code)] pub Caller);

impl FromRequest for AuthenticatedRequest {
    type Error = Error;
    type Future = future::Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        // Absent means the route resolved to a public scope, or the middleware
        // was not mounted. Either way the handler must not run: it is only
        // reachable here because it asked for proof of authentication.
        let result = req
            .extensions()
            .get::<Caller>()
            .cloned()
            .map(|caller| {
                // The audit breadcrumb: which credential acted, on what. The
                // access log records the request; this records the identity
                // behind it, including whether the retiring shared key was used.
                tracing::debug!(
                    principal = %caller.principal,
                    source = ?caller.source,
                    legacy = caller.legacy,
                    path = %req.path(),
                    "authenticated request"
                );
                AuthenticatedRequest(caller)
            })
            .ok_or_else(|| {
                actix_web::error::InternalError::from_response(
                    "Unauthenticated",
                    HttpResponse::Unauthorized().json(serde_json::json!({
                        "error": {
                            "code": "UNAUTHORIZED",
                            "message": "Authentication required"
                        }
                    })),
                )
                .into()
            });

        future::ready(result)
    }
}

/// Extracts workspace context from X-Org-Id and X-Workspace-Id headers,
/// resolves the schema_name from the database.
pub struct Workspace(pub WorkspaceContext);

impl FromRequest for Workspace {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let org_id = req
            .headers()
            .get("x-org-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let workspace_id = req
            .headers()
            .get("x-workspace-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let state = req.app_data::<web::Data<AppState>>().cloned();

        // Check if already resolved and stored in extensions
        if let Some(ctx) = req.extensions().get::<WorkspaceContext>().cloned() {
            return Box::pin(future::ready(Ok(Workspace(ctx))));
        }

        Box::pin(async move {
            let (org_id, workspace_id) = match (org_id, workspace_id) {
                (Some(o), Some(w)) => (o, w),
                _ => {
                    return Err(actix_web::error::InternalError::from_response(
                        "Missing workspace headers",
                        HttpResponse::BadRequest().json(serde_json::json!({
                            "error": {
                                "code": "MISSING_WORKSPACE",
                                "message": "X-Org-Id and X-Workspace-Id headers are required"
                            }
                        })),
                    )
                    .into())
                }
            };

            let state = state.ok_or_else(|| {
                actix_web::error::InternalError::from_response(
                    "Internal error",
                    HttpResponse::InternalServerError().finish(),
                )
            })?;

            let schema_name = invokr_common::db::workspaces::resolve_schema(
                &state.pool,
                &org_id,
                &workspace_id,
            )
            .await
            .map_err(|_| {
                actix_web::error::InternalError::from_response(
                    "Database error",
                    HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": { "code": "INTERNAL_ERROR", "message": "Failed to resolve workspace" }
                    })),
                )
            })?
            .ok_or_else(|| {
                actix_web::error::InternalError::from_response(
                    "Workspace not found",
                    HttpResponse::NotFound().json(serde_json::json!({
                        "error": {
                            "code": "WORKSPACE_NOT_FOUND",
                            "message": format!("Workspace {} not found in org {}", workspace_id, org_id)
                        }
                    })),
                )
            })?;

            Ok(Workspace(WorkspaceContext {
                org_id,
                workspace_id,
                schema_name,
            }))
        })
    }
}

/// Typed, validated jobs-list filters parsed from the query string.
///
/// Multi-value filters (`status`, `trigger_type`, `endpoint_type`) accept both
/// repeated params (`?status=A&status=B`, from the SDK) and comma-separated
/// (`?status=A,B`, from the dashboard); an invalid token is a 400. Wrapping the
/// parsing in a `FromRequest` extractor keeps the handler signature typed.
pub struct JobFilters(pub db::jobs::JobFilters);

impl FromRequest for JobFilters {
    type Error = Error;
    type Future = future::Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        // Raw pairs: a repeated key can't be folded into a `Vec` by the
        // struct-based `Query` deserializer. An unparseable query is a client
        // error (400) — never fall back to "no filters", which would quietly
        // return an unfiltered list the caller never asked for.
        let filters = web::Query::<Vec<(String, String)>>::from_query(req.query_string())
            .map(web::Query::into_inner)
            .map_err(|e| AppError::InvalidRequest(format!("Invalid query string: {e}")))
            .and_then(|pairs| parse_job_filters(&pairs))
            .map(JobFilters)
            .map_err(Into::into);
        future::ready(filters)
    }
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Last non-blank value for `key`; a repeated scalar key keeps the final one.
fn last_scalar(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs
        .iter()
        .filter(|(k, _)| k == key)
        .filter_map(|(_, v)| blank_to_none(Some(v.clone())))
        .next_back()
}

/// Validated, de-duplicated enum list for `key`. Accepts repeated params
/// (`?status=A&status=B`, from the SDK) or comma-separated (`?status=A,B`, from
/// the dashboard); an invalid token is a 400.
fn parse_filter_list<T: PartialEq>(
    pairs: &[(String, String)],
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
    label: &str,
) -> Result<Vec<T>, AppError> {
    let mut out: Vec<T> = Vec::new();
    for (_, value) in pairs.iter().filter(|(k, _)| k == key) {
        for token in value.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let parsed = parse(token)
                .ok_or_else(|| AppError::InvalidRequest(format!("Invalid {label}: {token}")))?;
            if !out.contains(&parsed) {
                out.push(parsed);
            }
        }
    }
    Ok(out)
}

/// Parses an optional RFC-3339 datetime query value to UTC; blank == absent.
fn parse_datetime(value: Option<String>, label: &str) -> Result<Option<DateTime<Utc>>, AppError> {
    match blank_to_none(value) {
        Some(s) => {
            let dt = DateTime::parse_from_rfc3339(&s)
                .map_err(|_| AppError::InvalidRequest(format!("Invalid {label}: {s}")))?;
            Ok(Some(dt.with_timezone(&Utc)))
        }
        None => Ok(None),
    }
}

/// Parses and validates the jobs-list filters from the raw query pairs. Enum
/// filters are validated up front (a typo is a 400, not silently zero rows).
fn parse_job_filters(pairs: &[(String, String)]) -> Result<db::jobs::JobFilters, AppError> {
    let created_after = parse_datetime(last_scalar(pairs, "created_after"), "created_after")?;
    let created_before = parse_datetime(last_scalar(pairs, "created_before"), "created_before")?;
    if let (Some(a), Some(b)) = (created_after, created_before) {
        if a > b {
            return Err(AppError::InvalidRequest(
                "created_after must not be after created_before".into(),
            ));
        }
    }
    Ok(db::jobs::JobFilters {
        job_id: last_scalar(pairs, "job_id"),
        status: parse_filter_list(pairs, "status", JobStatus::from_str_val, "status")?,
        trigger: parse_filter_list(pairs, "trigger_type", TriggerType::from_str_val, "trigger")?,
        endpoint: last_scalar(pairs, "endpoint"),
        endpoint_type: parse_filter_list(
            pairs,
            "endpoint_type",
            EndpointType::from_str_val,
            "endpoint_type",
        )?,
        created_after,
        created_before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds raw query pairs from `key=value` strings.
    fn pairs(kvs: &[(&str, &str)]) -> Vec<(String, String)> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn assert_invalid_request(result: Result<db::jobs::JobFilters, AppError>) {
        match result {
            Err(AppError::InvalidRequest(_)) => {}
            Err(_) => panic!("expected InvalidRequest"),
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }

    #[test]
    fn parse_job_filters_carries_trimmed_job_id() {
        let f = parse_job_filters(&pairs(&[("job_id", "  job-42  ")])).unwrap();
        assert_eq!(f.job_id, Some("job-42".to_string()));
    }

    #[test]
    fn parse_job_filters_parses_comma_separated_enums() {
        let f = parse_job_filters(&pairs(&[
            ("status", "ACTIVE,RETIRED"),
            ("trigger_type", "CRON,DELAYED"),
            ("endpoint", "notify"),
            ("endpoint_type", "HTTP,INTERNAL"),
        ]))
        .unwrap();
        assert_eq!(f.status, vec![JobStatus::ACTIVE, JobStatus::RETIRED]);
        assert_eq!(f.trigger, vec![TriggerType::CRON, TriggerType::DELAYED]);
        assert_eq!(f.endpoint, Some("notify".to_string()));
        assert_eq!(
            f.endpoint_type,
            vec![EndpointType::HTTP, EndpointType::INTERNAL]
        );
    }

    #[test]
    fn parse_job_filters_parses_repeated_params() {
        // The generated Smithy SDK emits one entry per list value.
        let f = parse_job_filters(&pairs(&[
            ("status", "ACTIVE"),
            ("status", "RETIRED"),
            ("endpoint_type", "HTTP"),
            ("endpoint_type", "INTERNAL"),
        ]))
        .unwrap();
        assert_eq!(f.status, vec![JobStatus::ACTIVE, JobStatus::RETIRED]);
        assert_eq!(
            f.endpoint_type,
            vec![EndpointType::HTTP, EndpointType::INTERNAL]
        );
    }

    #[test]
    fn parse_job_filters_mixes_repeated_and_comma_separated() {
        let f = parse_job_filters(&pairs(&[
            ("status", "ACTIVE,RETIRED"),
            ("status", "ACTIVE"),
        ]))
        .unwrap();
        // Deduped across both the comma-split and the repeated occurrence.
        assert_eq!(f.status, vec![JobStatus::ACTIVE, JobStatus::RETIRED]);
    }

    #[test]
    fn parse_job_filters_scalar_takes_last_non_blank() {
        let f = parse_job_filters(&pairs(&[("job_id", "job-1"), ("job_id", "  job-2  ")])).unwrap();
        assert_eq!(f.job_id, Some("job-2".to_string()));
    }

    #[test]
    fn parse_job_filters_trims_dedupes_and_drops_blanks() {
        let f = parse_job_filters(&pairs(&[("status", " ACTIVE , ACTIVE ,, RETIRED ")])).unwrap();
        assert_eq!(f.status, vec![JobStatus::ACTIVE, JobStatus::RETIRED]);
    }

    #[test]
    fn parse_job_filters_empty_lists_when_absent() {
        let f = parse_job_filters(&pairs(&[])).unwrap();
        assert!(f.status.is_empty());
        assert!(f.trigger.is_empty());
        assert!(f.endpoint_type.is_empty());
        assert_eq!(f.job_id, None);
        assert_eq!(f.endpoint, None);
        assert_eq!(f.created_after, None);
        assert_eq!(f.created_before, None);
    }

    #[test]
    fn parse_job_filters_blank_values_are_absent() {
        // The dashboard sends empty params for an "All" selection.
        let f = parse_job_filters(&pairs(&[
            ("status", ""),
            ("job_id", "  "),
            ("endpoint", ""),
        ]))
        .unwrap();
        assert!(f.status.is_empty());
        assert_eq!(f.job_id, None);
        assert_eq!(f.endpoint, None);
    }

    #[test]
    fn parse_job_filters_parses_rfc3339_dates() {
        let f = parse_job_filters(&pairs(&[
            ("created_after", "2026-06-18T00:00:00Z"),
            ("created_before", "2026-06-24T23:59:59Z"),
        ]))
        .unwrap();
        assert!(f.created_after.is_some());
        assert!(f.created_before.is_some());
    }

    #[test]
    fn parse_job_filters_rejects_bad_enum_token() {
        assert_invalid_request(parse_job_filters(&pairs(&[("status", "ACTIVE,BOGUS")])));
    }

    #[test]
    fn parse_job_filters_rejects_bad_date() {
        assert_invalid_request(parse_job_filters(&pairs(&[("created_after", "yesterday")])));
    }

    #[test]
    fn parse_job_filters_rejects_inverted_date_range() {
        assert_invalid_request(parse_job_filters(&pairs(&[
            ("created_after", "2026-06-24T00:00:00Z"),
            ("created_before", "2026-06-18T00:00:00Z"),
        ])));
    }

    /// Parse a raw query string exactly as the extractor does.
    fn from_query(q: &str) -> Vec<(String, String)> {
        web::Query::<Vec<(String, String)>>::from_query(q)
            .map(web::Query::into_inner)
            .expect("query must parse")
    }

    /// No query string at all means "no filters" — never an error. Guards the
    /// 400-on-unparseable path from regressing into rejecting plain requests.
    #[test]
    fn empty_query_string_yields_no_filters() {
        let f = parse_job_filters(&from_query("")).unwrap();
        assert!(f.status.is_empty());
        assert!(f.trigger.is_empty());
        assert!(f.endpoint_type.is_empty());
        assert_eq!(f.job_id, None);
        assert_eq!(f.endpoint, None);
    }

    /// A trailing (or doubled) `&` leaves an empty segment that the parser
    /// drops; the real filters must still resolve.
    #[test]
    fn stray_ampersands_do_not_drop_filters() {
        let f =
            parse_job_filters(&from_query("limit=50&status=ACTIVE&endpoint_type=KAFKA&")).unwrap();
        assert_eq!(f.status, vec![JobStatus::ACTIVE]);
        assert_eq!(f.endpoint_type, vec![EndpointType::KAFKA]);

        let f = parse_job_filters(&from_query("limit=50&&status=ACTIVE")).unwrap();
        assert_eq!(f.status, vec![JobStatus::ACTIVE]);
    }
}
