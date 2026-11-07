#[cfg(feature = "actix")]
use actix_web::{http::StatusCode, HttpResponse, ResponseError};
#[cfg(feature = "actix")]
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Payload spec not found: {0}")]
    PayloadSpecNotFound(String),
    #[error("Config not found: {0}")]
    ConfigNotFound(String),
    #[error("Secret not found: {0}")]
    SecretNotFound(String),
    #[error("Endpoint not found: {0}")]
    EndpointNotFound(String),
    #[error("Job not found: {0}")]
    JobNotFound(String),
    #[error("Execution not found: {0}")]
    ExecutionNotFound(String),
    #[error("Organization not found: {0}")]
    OrgNotFound(String),
    #[error("Workspace not found: {0}")]
    WorkspaceNotFound(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Job not updatable: {0}")]
    JobNotUpdatable(String),
    #[error("Execution not cancellable: {0}")]
    ExecutionNotCancellable(String),
    #[error("Invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("Invalid schema: {0}")]
    InvalidSchema(String),
    #[error("Invalid payload spec reference: {0}")]
    InvalidPayloadSpecRef(String),
    #[error("Invalid config reference: {0}")]
    InvalidConfigRef(String),
    #[error("Input validation failed: {0}")]
    InputValidationFailed(String),
    #[error("Template resolution failed: {0}")]
    TemplateResolutionFailed(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(feature = "actix")]
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[cfg(feature = "actix")]
#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    request_id: Option<String>,
}

#[cfg(feature = "actix")]
impl AppError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "INVALID_REQUEST"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED"),
            Self::PayloadSpecNotFound(_) => (StatusCode::NOT_FOUND, "PAYLOAD_SPEC_NOT_FOUND"),
            Self::ConfigNotFound(_) => (StatusCode::NOT_FOUND, "CONFIG_NOT_FOUND"),
            Self::SecretNotFound(_) => (StatusCode::NOT_FOUND, "SECRET_NOT_FOUND"),
            Self::EndpointNotFound(_) => (StatusCode::NOT_FOUND, "ENDPOINT_NOT_FOUND"),
            Self::JobNotFound(_) => (StatusCode::NOT_FOUND, "JOB_NOT_FOUND"),
            Self::ExecutionNotFound(_) => (StatusCode::NOT_FOUND, "EXECUTION_NOT_FOUND"),
            Self::OrgNotFound(_) => (StatusCode::NOT_FOUND, "ORG_NOT_FOUND"),
            Self::WorkspaceNotFound(_) => (StatusCode::NOT_FOUND, "WORKSPACE_NOT_FOUND"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "CONFLICT"),
            Self::JobNotUpdatable(_) => (StatusCode::CONFLICT, "JOB_NOT_UPDATABLE"),
            Self::ExecutionNotCancellable(_) => (StatusCode::CONFLICT, "EXECUTION_NOT_CANCELLABLE"),
            Self::InvalidCron(_) => (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_CRON"),
            Self::InvalidSchema(_) => (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_SCHEMA"),
            Self::InvalidPayloadSpecRef(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_PAYLOAD_SPEC_REF")
            }
            Self::InvalidConfigRef(_) => (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_CONFIG_REF"),
            Self::InputValidationFailed(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "INPUT_VALIDATION_FAILED")
            }
            Self::TemplateResolutionFailed(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "TEMPLATE_RESOLUTION_FAILED",
            ),
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        }
    }
}

#[cfg(feature = "actix")]
impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        self.status_and_code().0
    }

    fn error_response(&self) -> HttpResponse {
        let (status, code) = self.status_and_code();
        if status.is_server_error() {
            tracing::error!(code, message = %self, "request error");
        } else {
            tracing::warn!(code, message = %self, "request error");
        }
        let body = ErrorBody {
            error: ErrorDetail {
                code,
                message: self.to_string(),
                request_id: None,
            },
        };
        HttpResponse::build(status).json(body)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "Database error");
        Self::Internal("Database error".into())
    }
}
