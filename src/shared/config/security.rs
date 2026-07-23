use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::shared::ComputerUseConfig;
use crate::shared::DockerConfig;

fn default_block_gitignored_dotfiles() -> bool {
    true
}

fn default_max_file_read_size() -> usize {
    1024 * 1024
}

fn default_max_overwrite_size() -> usize {
    1024 * 1024
}

fn default_bash_sandbox_workdir() -> bool {
    true
}

fn default_commit_max_file_size() -> u64 {
    5 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default)]
    pub permission_rules: Vec<crate::shared::permission::PermissionRule>,
    #[serde(default)]
    pub deny_paths: Vec<String>,
    #[serde(default)]
    pub deny_urls: Vec<String>,
    #[serde(default)]
    pub deny_extensions: Vec<String>,
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
    #[serde(default)]
    pub sandbox_dir: Option<String>,
    #[serde(default)]
    pub block_dotfiles: bool,
    #[serde(default = "default_block_gitignored_dotfiles")]
    pub block_gitignored_dotfiles: bool,
    #[serde(default = "default_max_file_read_size")]
    pub max_file_read_size: usize,
    #[serde(default = "default_max_overwrite_size")]
    pub max_overwrite_size: usize,
    #[serde(default = "default_bash_sandbox_workdir")]
    pub bash_sandbox_workdir: bool,
    #[serde(default)]
    pub bang_requires_approval: bool,
    #[serde(default = "default_commit_max_file_size")]
    pub commit_max_file_size: u64,
    #[serde(default)]
    pub computer_use: ComputerUseConfig,
    #[serde(default)]
    pub docker: DockerConfig,
    #[serde(default)]
    pub audit_log_path: Option<PathBuf>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            auto_approve: false,
            permission_rules: vec![],
            deny_paths: vec![],
            deny_urls: vec![],
            deny_extensions: vec![],
            allowed_write_dirs: vec![],
            sandbox_dir: None,
            block_dotfiles: false,
            block_gitignored_dotfiles: default_block_gitignored_dotfiles(),
            max_file_read_size: default_max_file_read_size(),
            max_overwrite_size: default_max_overwrite_size(),
            bash_sandbox_workdir: default_bash_sandbox_workdir(),
            bang_requires_approval: false,
            commit_max_file_size: default_commit_max_file_size(),
            computer_use: ComputerUseConfig::default(),
            docker: DockerConfig::default(),
            audit_log_path: None,
        }
    }
}
