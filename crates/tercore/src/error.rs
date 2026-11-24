//! Error types shared by every TerCode subsystem.
//!
//! Errors are deliberately descriptive: the UI surfaces `Display` verbatim, so
//! each variant explains what failed and what the user can do about it.

use serde::Serialize;

pub type Result<T> = std::result::Result<T, TerError>;

#[derive(Debug, thiserror::Error)]
pub enum TerError {
    #[error("Ter Router rejected the API key ({status}). Check the key in Settings → AI Provider.")]
    Unauthorized { status: u16 },

    #[error("Ter Router rate limit reached (429). Retry in {retry_after_secs}s or switch models.")]
    RateLimited { retry_after_secs: u64 },

    #[error("Ter Router API error {status}: {message}")]
    Api { status: u16, message: String },

    #[error("No Ter Router API key is configured. Add one in Settings → AI Provider.")]
    MissingApiKey,

    #[error("Network error talking to {context}: {source}")]
    Network {
        context: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("Ter Router request timed out after {secs}s")]
    Timeout { secs: u64 },

    #[error("Ter Router returned a malformed response: {0}")]
    BadResponse(String),

    #[error("No model is selected. Pick one in the model selector.")]
    NoModelSelected,

    #[error("MCP: {0}")]
    Mcp(String),

    #[error("MCP server exited unexpectedly (code {code:?}). {hint}")]
    McpExited { code: Option<i32>, hint: String },

    #[error("Roblox Studio MCP server was not found. {0}")]
    McpNotFound(String),

    #[error("Roblox Studio is not running. Open Studio and try again.")]
    StudioNotRunning,

    #[error("Roblox Studio is not installed on this machine.")]
    StudioNotInstalled,

    #[error("I/O error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Invalid JSON in {context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Credential Manager error: {0}")]
    Secret(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Invalid project file name: {0}")]
    InvalidPath(String),

    #[error("{0}")]
    Invalid(String),

    #[error("The operation was cancelled")]
    Cancelled,
}

impl TerError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        TerError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        TerError::Json {
            context: context.into(),
            source,
        }
    }

    pub fn mcp(msg: impl Into<String>) -> Self {
        TerError::Mcp(msg.into())
    }

    /// A short machine-readable code the UI uses to pick an icon and retry hint.
    pub fn code(&self) -> &'static str {
        match self {
            TerError::Unauthorized { .. } => "unauthorized",
            TerError::RateLimited { .. } => "rate_limited",
            TerError::Api { .. } => "api",
            TerError::MissingApiKey => "missing_api_key",
            TerError::Network { .. } => "network",
            TerError::Timeout { .. } => "timeout",
            TerError::BadResponse(_) => "bad_response",
            TerError::NoModelSelected => "no_model",
            TerError::Mcp(_) => "mcp",
            TerError::McpExited { .. } => "mcp_exited",
            TerError::McpNotFound(_) => "mcp_not_found",
            TerError::StudioNotRunning => "studio_not_running",
            TerError::StudioNotInstalled => "studio_not_installed",
            TerError::Io { .. } => "io",
            TerError::Json { .. } => "json",
            TerError::Secret(_) => "secret",
            TerError::ProjectNotFound(_) => "project_not_found",
            TerError::InvalidPath(_) => "invalid_path",
            TerError::Invalid(_) => "invalid",
            TerError::Cancelled => "cancelled",
        }
    }

    /// Whether the caller may transparently retry the operation.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            TerError::RateLimited { .. } | TerError::Network { .. } | TerError::Timeout { .. }
        ) || matches!(self, TerError::Api { status, .. } if *status >= 500)
    }
}

/// Wire representation handed to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl From<&TerError> for ErrorPayload {
    fn from(e: &TerError) -> Self {
        Self {
            code: e.code(),
            message: e.to_string(),
            retryable: e.retryable(),
        }
    }
}

impl From<TerError> for ErrorPayload {
    fn from(e: TerError) -> Self {
        (&e).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(TerError::RateLimited {
            retry_after_secs: 3
        }
        .retryable());
        assert!(TerError::Api {
            status: 503,
            message: "upstream".into()
        }
        .retryable());
        assert!(!TerError::Api {
            status: 400,
            message: "bad".into()
        }
        .retryable());
        assert!(!TerError::MissingApiKey.retryable());
    }

    #[test]
    fn payload_carries_code_and_message() {
        let payload: ErrorPayload = TerError::StudioNotRunning.into();
        assert_eq!(payload.code, "studio_not_running");
        assert!(payload.message.contains("Studio"));
    }
}
