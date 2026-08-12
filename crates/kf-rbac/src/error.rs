//! Auth errors mirroring `@kirkforge/core-errors` AuthError codes.

/// The four AuthError code strings from the TS source, as a type-safe enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorCode {
    Unauthorized,
    Forbidden,
    InvalidToken,
    MethodNotAllowed,
}

impl AuthErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::InvalidToken => "INVALID_TOKEN",
            Self::MethodNotAllowed => "METHOD_NOT_ALLOWED",
        }
    }
}

/// Auth error: code + message + structured context. Mirrors the TS `AuthError`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthError {
    pub code: AuthErrorCode,
    pub message: String,
    pub context: serde_json::Value,
}

impl AuthError {
    pub fn new(
        code: AuthErrorCode,
        message: impl Into<String>,
        context: serde_json::Value,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            context,
        }
    }

    pub fn invalid_token(message: impl Into<String>) -> Self {
        Self::new(
            AuthErrorCode::InvalidToken,
            message,
            serde_json::Value::Object(serde_json::Map::new()),
        )
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(
            AuthErrorCode::Forbidden,
            message,
            serde_json::Value::Object(serde_json::Map::new()),
        )
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(
            AuthErrorCode::Unauthorized,
            message,
            serde_json::Value::Object(serde_json::Map::new()),
        )
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AuthError {}
