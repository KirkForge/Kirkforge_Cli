//! MCP client error type. Extracted from the parent module so the
//! transport/request lifecycle stays focused on framing and matching.

/// Errors that can occur when sending a JSON-RPC request to an MCP
/// server.
#[derive(Debug)]
pub(super) enum McpError {
    /// The request could not be written to the server's stdin, or
    /// the server closed its stdin pipe.
    Io(std::io::Error),
    /// The server did not produce a response within `REQUEST_TIMEOUT`.
    Timeout,
    /// The server returned a JSON-RPC error object.
    JsonRpc { code: i64, message: String },
    /// The response channel closed before a response arrived (server
    /// process likely exited).
    ChannelClosed,
    /// The client has been disconnected or the server process exited.
    Disconnected,
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Io(e) => write!(f, "I/O error: {e}"),
            McpError::Timeout => write!(f, "request timed out"),
            McpError::JsonRpc { code, message } => {
                write!(f, "JSON-RPC error {code}: {message}")
            }
            McpError::ChannelClosed => write!(f, "response channel closed"),
            McpError::Disconnected => write!(f, "MCP client disconnected"),
        }
    }
}

impl From<std::io::Error> for McpError {
    fn from(err: std::io::Error) -> Self {
        McpError::Io(err)
    }
}
