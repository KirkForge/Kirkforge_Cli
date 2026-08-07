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

impl std::error::Error for McpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            McpError::Io(e) => Some(e),
            McpError::Timeout
            | McpError::JsonRpc { .. }
            | McpError::ChannelClosed
            | McpError::Disconnected => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn io_error_display_and_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let mcp = McpError::Io(io_err);
        let msg = mcp.to_string();
        assert!(msg.contains("pipe broke"), "display should include io error: {msg}");
        assert!(mcp.source().is_some(), "Io variant should have a source");
    }

    #[test]
    fn timeout_display_no_source() {
        let mcp = McpError::Timeout;
        let msg = mcp.to_string();
        assert!(msg.contains("timed out"), "display: {msg}");
        assert!(mcp.source().is_none());
    }

    #[test]
    fn json_rpc_display() {
        let mcp = McpError::JsonRpc { code: -32600, message: "invalid".into() };
        let msg = mcp.to_string();
        assert!(msg.contains("-32600"), "display: {msg}");
        assert!(msg.contains("invalid"), "display: {msg}");
        assert!(mcp.source().is_none());
    }

    #[test]
    fn channel_closed_display() {
        let msg = McpError::ChannelClosed.to_string();
        assert!(msg.contains("channel closed"), "display: {msg}");
    }

    #[test]
    fn disconnected_display() {
        let msg = McpError::Disconnected.to_string();
        assert!(msg.contains("disconnected"), "display: {msg}");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let mcp: McpError = io_err.into();
        assert!(matches!(mcp, McpError::Io(_)));
        assert!(mcp.to_string().contains("not found"));
    }
}

impl From<std::io::Error> for McpError {
    fn from(err: std::io::Error) -> Self {
        McpError::Io(err)
    }
}
