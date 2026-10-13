//! Shared endpoint-registration guards, used by the REST handlers and the
//! library client alike so the two cannot drift on what may be registered.

use crate::db::{self, DbContext};
use crate::error::AppError;
use crate::models::endpoint::EndpointType;

/// Reject endpoint types a caller may not register. `INTERNAL` endpoints back
/// invokr's own jobs (today: the dogfooded reaper) and are provisioned with the
/// workspace, never registered by a caller; rejecting them here keeps the CHECK
/// constraint from doing it with an opaque error.
pub fn ensure_user_endpoint_type(endpoint_type: &str) -> Result<(), AppError> {
    match EndpointType::from_str_val(endpoint_type) {
        None | Some(EndpointType::INTERNAL) => Err(AppError::InvalidRequest(format!(
            "Invalid endpoint type: {endpoint_type}. Must be HTTP, KAFKA, or REDIS_STREAM"
        ))),
        _ => Ok(()),
    }
}

/// Reject changes to an existing `INTERNAL` endpoint. Its job (today: the
/// reaper) keeps firing against whatever spec replaces its own, so the
/// workspace's sweep would stop with nothing to report it — and nothing to
/// re-provision it.
pub fn ensure_user_managed(existing_endpoint_type: &str) -> Result<(), AppError> {
    if EndpointType::from_str_val(existing_endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::Conflict(
            "Internal invokr endpoints cannot be modified through the API".into(),
        ));
    }
    Ok(())
}

/// Check that the payload spec and config an endpoint refers to exist, so a typo
/// fails at registration rather than at the first job that uses the endpoint.
pub async fn check_refs(
    db: &mut DbContext<'_>,
    payload_spec_ref: Option<&str>,
    config_ref: Option<&str>,
) -> Result<(), AppError> {
    if let Some(ps) = payload_spec_ref {
        if db::payload_specs::get(db, ps).await?.is_none() {
            return Err(AppError::InvalidPayloadSpecRef(ps.to_string()));
        }
    }
    if let Some(cfg) = config_ref {
        if db::configs::get(db, cfg).await?.is_none() {
            return Err(AppError::InvalidConfigRef(cfg.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_endpoints_cannot_be_modified() {
        let err = ensure_user_managed("INTERNAL").unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn user_endpoints_can_be_modified() {
        for t in ["HTTP", "KAFKA", "REDIS_STREAM"] {
            assert!(ensure_user_managed(t).is_ok(), "{t}");
        }
    }

    #[test]
    fn user_endpoint_types_are_accepted() {
        for t in ["HTTP", "KAFKA", "REDIS_STREAM"] {
            assert!(ensure_user_endpoint_type(t).is_ok(), "{t}");
        }
    }

    #[test]
    fn internal_endpoints_cannot_be_registered() {
        let err = ensure_user_endpoint_type("INTERNAL").unwrap_err();
        assert!(matches!(err, AppError::InvalidRequest(_)), "{err:?}");
    }

    #[test]
    fn unknown_types_are_rejected_with_the_rest_message() {
        let err = ensure_user_endpoint_type("http").unwrap_err();
        assert_eq!(
            err.to_string(),
            AppError::InvalidRequest(
                "Invalid endpoint type: http. Must be HTTP, KAFKA, or REDIS_STREAM".into()
            )
            .to_string()
        );
    }
}
