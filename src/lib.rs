mod alternates;
mod anubis;
mod cache;
pub mod clearance;
pub mod fetch;
mod focus;
pub mod mcp;
mod pdf;
pub mod render;
pub mod search;
pub mod transport;

use serde::Serialize;

pub(crate) fn retrieved_at() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("current UTC time fits RFC 3339")
}

#[derive(Debug, Serialize)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<String>,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            provider: None,
            status: None,
            retry_after: None,
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<wreq::Error> for ToolError {
    fn from(error: wreq::Error) -> Self {
        if error.is_timeout() {
            return Self::new("timeout", "The site did not answer within 30 seconds");
        }
        Self::new("network_error", error.to_string())
    }
}
