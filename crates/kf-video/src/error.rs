use thiserror::Error;

#[derive(Debug, Error)]
pub enum KfError {
    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),

    #[error("tool not found: {0}")]
    ToolMissing(String),

    #[error("invalid artifact: {0}")]
    Artifact(String),

    #[error("checkpoint error: {0}")]
    Checkpoint(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("other: {0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, KfError>;
