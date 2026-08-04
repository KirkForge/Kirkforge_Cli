// `kf-code run` session setup: `RunArgs` + `run_session` + the
// recent-sessions hint helper. Extracted from the binary root — pure
// move, no behaviour change.

use super::line_mode::run_line_mode;
use super::turn_events::resolve_continue_path;
use kf_code::{adapters, daemon, line_mode, session, tools, tui};
use std::io::IsTerminal;

pub(crate) struct RunArgs {
    pub(crate) model: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) model_type: Option<String>,
    pub(crate) auto_approve: bool,
    pub(crate) dry_run: bool,
    pub(crate) system: Option<String>,
    pub(crate) resume: Option<String>,
    pub(crate) non_interactive: bool,
    pub(crate) output: kf_code::shared::OutputFormat,
    pub(crate) max_turns: usize,
    pub(crate) continue_session: Option<String>,
    pub(crate) auto_resume: bool,
    pub(crate) attach: Option<String>,
    pub(crate) no_tui: bool,
    pub(crate) seed: Option<u64>,
    pub(crate) worktree: bool,
    pub(crate) docker: bool,
    pub(crate) harden: bool,
    pub(crate) no_trace: bool,
}

pub(super) async fn run_session(args: RunArgs) -> anyhow::Result<()> {
    let RunArgs {
        model,
        host,
        model_type,
        auto_approve,
        dry_run,
        system,
        resume,
        non_interactive,
        output,
        max_turns,
        continue_session,
        auto_resume,
        attach,
        no_tui,
        seed,
        worktree,
        docker,
        harden,
        no_trace,
    } = args;

    let mut config = session::config::load_or_create_config();

    if let Some(host) = &host {
        config.model.ollama_host = host.clone();
    }
    let model = model.unwrap_or_else(|| config.model.default_model.clone());
    if auto_approve {
        config.security.auto_approve = true;
    }
    if dry_run {
        config.tools.dry_run = true;
    }
    if let Some(seed) = seed {
        config.model.seed = Some(seed);
    }
    if worktree {
        config.session.worktree_enabled = true;
    }
    if docker {
        config.security.docker.enabled = true;
    }
    if harden {
        config.security.sandbox.harden = true;
    }
    let trace_enabled = !no_trace;

    // CLI flags are transient runtime overrides; do not persist them to
    // config.toml. `load_or_create_config` already wrote a default file on
    // first run, and explicit in-session config changes are saved by their
    // respective handlers (e.g. /reload). Persisting here made a single
    // scripted invocation permanently flip `auto_approve` or `dry_run`.
    //
    // We keep the loaded/merged config object for the rest of the session.
    //
    // Previously: `session::config::save_config(&config)` was called here.

    // Honor `NO_COLOR` / `TERM=dumb` consistently across all user-facing
    // output, including the session-restoration message printed before the
    // TUI/line-mode branch is chosen.
    let no_color =
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").is_ok_and(|t| t == "dumb");

    // Resolve the launch-time cwd exactly once, then freeze it on the
    // Config. Review.md arch concern #3: previously, `Config::default()`
    // did this resolution, which (a) ran before any validation, and
    // (b) allowed a deletion-after-launch race to silently widen the
    // sandbox to `None`. `freeze_launch_sandbox` is the new single
    // resolution site: resolves `current_dir()` once, captures the
    // value, and honors the operator's explicit-escape-hatch policy.
    let _frozen_cwd = session::config::freeze_launch_sandbox(&mut config);

    let ollama_host = &config.model.ollama_host;

    let data_dir = session::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let session_id = session::new_session_id();

    // ── Git worktree (--worktree flag) ──
    // When enabled, create an isolated git worktree for the session.
    // Edits land in the worktree, not the user's working tree.
    // The worktree is removed when `_worktree` is dropped.
    let _worktree: Option<session::worktree::WorktreeSession> = if config.session.worktree_enabled {
        let repo_root = std::env::current_dir()?;
        let wt = session::worktree::WorktreeSession::create(&session_id.to_string(), &repo_root)?;
        // Redirect sandbox to the worktree path
        config.security.sandbox_dir = Some(wt.path().to_string_lossy().to_string());
        // Also redirect the log path into the worktree
        Some(wt)
    } else {
        None
    };

    // Resolve the log path. Priority order:
    //   1. `--continue-session <value>` — id prefix OR full path
    //   2. `--resume <path>`            — legacy path-only flag
    //   3. `--attach <id-or-prefix>`    — via session daemon
    //   4. `--auto-resume`              — most recent session via daemon
    //   5. TUI startup picker (if daemon has recent sessions)
    //   6. brand-new session id
    let log_path = if let Some(cont) = &continue_session {
        resolve_continue_path(cont)?
    } else if let Some(resume) = &resume {
        std::path::PathBuf::from(resume)
    } else if let Some(id) = &attach {
        match daemon::client::try_resolve_id(id).await? {
            Some(path) => path,
            None => {
                anyhow::bail!(
                    "daemon could not resolve session '{id}'. Run `/sessions` to see available ids."
                );
            }
        }
    } else if auto_resume {
        match daemon::client::try_resolve_recent().await? {
            Some(path) => {
                tracing::info!(path = %path.display(), "auto-resuming most recent session");
                path
            }
            None => {
                tracing::info!("no recent sessions found; starting a new session");
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
            }
        }
    } else {
        // Try the daemon for a startup picker in TUI mode, or a hint in
        // non-interactive / no-TUI mode.
        match daemon::client::try_list_recent().await? {
            Some(sessions) if !sessions.is_empty() && !non_interactive && !no_tui => {
                match tui::run_session_picker(sessions).await? {
                    Some(path) => {
                        tracing::info!(path = %path.display(), "resuming selected session");
                        path
                    }
                    None => {
                        tracing::info!("user chose new session");
                        let sessions_dir = data_dir.join("sessions");
                        std::fs::create_dir_all(&sessions_dir)?;
                        sessions_dir.join(format!("{session_id}.conv.ndjson"))
                    }
                }
            }
            Some(sessions) if !sessions.is_empty() => {
                // In machine-readable output modes the hint would pollute
                // stderr that callers may capture; only show it in plain
                // text mode where a human is reading the terminal.
                if output == kf_code::shared::OutputFormat::Text {
                    print_recent_sessions_hint(&sessions);
                }
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
            }
            _ => {
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
            }
        }
    };

    // Tell the daemon this session is now active.
    let touch_id = log_path
        .file_stem()
        .and_then(|f| f.to_str())
        .map(|s| s.trim_end_matches(".conv").to_string())
        .unwrap_or_else(|| session_id.to_string());
    daemon::client::try_touch(&touch_id, log_path.clone()).await;
    kf_code::session::session_index::touch_session(&touch_id, &log_path);

    let (mut conversation, open_outcome) =
        session::conversation::ConversationLog::open(log_path.clone())?;
    conversation =
        conversation.with_checkpoint_interval(config.session.checkpoint_interval_messages);
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        let warn_icon = line_mode::symbol(no_color, "⚠️");
        let warn_sep = if warn_icon.is_empty() { "" } else { " " };
        eprintln!("{warn_icon}{warn_sep}Session log was corrupt; restored {messages} message(s) from checkpoint.");
    }

    // ── Turn trace recorder ──
    // Persists TurnRecords alongside the conversation log for replay.
    // Disabled by --no-trace; when enabled, records one line per turn.
    let trace_recorder = if trace_enabled {
        let trace_path = log_path.with_extension("trace.ndjson");
        match session::replay::TraceRecorder::open(&trace_path) {
            Ok(rec) => Some(rec),
            Err(e) => {
                tracing::warn!(error = %e, path = %trace_path.display(), "could not open trace file; continuing without tracing");
                None
            }
        }
    } else {
        None
    };

    let adapter = adapters::caching::maybe_wrap_cached(
        adapters::adapter_for_with_provider(
            &model,
            ollama_host,
            model_type.as_deref(),
            &config.model.anthropic_provider,
            config.model.request_timeout_secs,
            &config.model.opencode_zen_endpoint,
            config.model.opencode_zen_api_key.as_deref(),
        ),
        &config,
    );

    // ── Undo stack (review.md gap #7) ──
    // Per-session edit undo. Constructed here so the EditFile and
    // WriteFile tools can capture pre-edit bytes for `/undo`.
    // Wrapped in `Arc<Mutex<_>>` because the executor and the TUI's
    // `/undo` handler both touch it. The critical sections are tiny
    // (push a snapshot, pop a file) so contention is not a concern.
    //
    // We log a warning and proceed without undo if the data dir
    // can't be resolved — better than refusing the edit.
    let undo_stack = match session::undo::UndoStack::for_session(&session_id.to_string()) {
        Ok(s) => Some(std::sync::Arc::new(std::sync::Mutex::new(s))),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = ?e,
                "could not open undo stack — edits will not be undoable this session"
            );
            None
        }
    };

    // ── Built-in tool access controls ──
    // PathGuard / DenyList are required by the bash, grep, and glob tools so
    // they can enforce sandbox containment and deny-list checks at the tool
    // layer (e.g. background bash must re-check the command, grep/glob must
    // re-check each discovered file). Build them once from the resolved
    // launch-time config.
    let (builtin_deny_list, builtin_path_guard, _builtin_read_gate) =
        session::access::access_from_config(&config);
    let bash_sandbox_workdir = config.security.bash_sandbox_workdir;
    let minify_write_side = config.tools.minify_write_side;
    let minify_above_bytes = config.tools.minify_above_bytes;
    let computer_use_cfg = config.security.computer_use.clone();
    let computer_use_enabled = computer_use_cfg.enabled;

    // ── LSP pool (lazy-started, fail-cooled) ──
    // Build the pool from `[[lsp_servers]]` config. Servers are spawned
    // lazily on the first `lsp_query` call for that language, so this is
    // cheap when no LSP-aware tool runs. The pool is wrapped in `Arc` and
    // shared with the `lsp_query` tool below.
    let lsp_pool: Option<std::sync::Arc<kf_lsp::LspPool>> =
        if config.tools.lsp_servers.is_empty() {
            None
        } else {
            let language_configs: Vec<kf_lsp::LanguageConfig> = config
                .tools
                .lsp_servers
                .iter()
                .map(|e| kf_lsp::LanguageConfig {
                    name: e.language.clone(),
                    extensions: e.extensions.clone(),
                    lsp: Some(kf_lsp::LspServerConfig {
                        command: e.command.clone(),
                        args: e.args.clone(),
                    }),
                })
                .collect();
            Some(std::sync::Arc::new(kf_lsp::LspPool::new(
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string()),
                language_configs,
            )))
        };

    // ── Chrome tab for computer_use ──
    // Try to launch Chrome only when the tool is enabled. If the launch fails,
    // fall back to a placeholder tab that fails gracefully at runtime. This
    // keeps the toolset construction cheap and avoids hard-failing startup
    // when Chrome is not installed.
    let chrome_tab: std::sync::Arc<dyn crate::tools::computer_use::ChromeTab> =
        if computer_use_enabled {
            match super::chrome_launcher::launch_chrome_tab(&config.security.computer_use).await {
                Ok(tab) => tab,
                Err(e) => {
                    tracing::warn!(error = %e, "computer_use enabled but Chrome launch failed; tool will fail gracefully");
                    std::sync::Arc::new(crate::tools::computer_use::PlaceholderTab)
                }
            }
        } else {
            std::sync::Arc::new(crate::tools::computer_use::PlaceholderTab)
        };

    // Session launcher for multi-step browser sessions. When Chrome is
    // available, each `open` action gets a fresh Browser process that
    // stays alive until `close` drops it.
    let session_launcher: Option<crate::tools::computer_use::SessionLauncher> =
        if computer_use_enabled {
            let cfg = config.security.computer_use.clone();
            Some(std::sync::Arc::new(move || {
                let cfg = cfg.clone();
                Box::pin(async move { super::chrome_launcher::open_browser_session(&cfg).await })
                    as crate::tools::computer_use::SessionFuture
            })
                as crate::tools::computer_use::SessionLauncher)
        } else {
            None
        };

    // ── Toolset assembly (Phase 2.2) ──
    // Compose built-in, MCP, and plugin tools into a single source-aware
    // collection. The executor receives the flattened vector, but order and
    // duplicate-name resolution are controlled here: built-ins win over MCP,
    // and MCP wins over plugins.
    let mut toolset = session::toolset::CompositeToolset::empty();
    toolset.add(Box::new(session::toolset::VecToolset::new(
        "builtin",
        tools::all_tools(
            undo_stack.clone(),
            adapter.model_info().supports_images,
            builtin_deny_list,
            builtin_path_guard,
            bash_sandbox_workdir,
            minify_write_side,
            minify_above_bytes,
            lsp_pool.clone(),
            Some((computer_use_enabled, computer_use_cfg.clone())),
            Some(chrome_tab),
            session_launcher,
            Some(config.security.docker.clone()),
            config.security.sandbox.clone(),
        ),
    )));

    // ── Shared config (hot-reload foundation) ──
    // Wrap the launch-time Config in an Arc<RwLock> so both TUI and
    // executor can observe live updates from SIGHUP or `/reload`.
    let shared_config = std::sync::Arc::new(std::sync::RwLock::new(config));

    // ── Repo-graph context index (P1-long-1) ──
    // Build a tree-sitter-backed symbol index from the sandbox directory.
    // The index is passed to the executor's PromptBuilder so relevant
    // symbols are injected into the system prompt before every turn.
    // ADR-037 Phase 4: disk caching at .kf-code/context-index/cache.json.
    // On subsequent runs, if the cached index matches the current git HEAD,
    // we load from disk instead of rebuilding.
    let context_index = {
        let cfg = kf_code::shared::read_shared_config(&shared_config);
        cfg.security.sandbox_dir.as_ref().and_then(|dir| {
            let path = std::path::Path::new(dir);
            if !path.is_dir() {
                return None;
            }
            let cache_path = path.join(".kf-code/context-index/cache.json");
            if cache_path.exists() {
                if let Ok(cached) = kf_context_index::ContextIndex::load(&cache_path) {
                    if kf_context_index::ContextIndex::is_current(&cached, path) {
                        let idx = kf_context_index::ContextIndex::from_symbols_and_edges_and_calls(cached.symbols, cached.edges, cached.call_edges);
                        tracing::info!(
                            symbol_count = idx.symbols().len(),
                            "loaded repo-graph context index from cache"
                        );
                        return Some(idx);
                    }
                    tracing::info!("context index cache is stale (HEAD mismatch), rebuilding");
                } else {
                    tracing::info!("context index cache is corrupt, rebuilding");
                }
            }
            let mut idx = kf_context_index::ContextIndex::new();
            match idx.index_dir(path) {
                Ok(()) => {
                    let count = idx.symbols().len();
                    tracing::info!(symbol_count = count, sandbox_dir = %dir, "built repo-graph context index");
                    let head = kf_context_index::current_head(path).unwrap_or_default();
                    if let Err(e) = idx.save(&cache_path, &head) {
                        tracing::warn!(error = %e, "failed to save context index cache");
                    }
                    Some(idx)
                }
                Err(e) => {
                    tracing::warn!(error = %e, sandbox_dir = %dir, "failed to build context index");
                    None
                }
            }
        })
    };

    // --- MCP tools ---
    let cfg_for_mcp = kf_code::shared::read_shared_config(&shared_config).clone();
    if !cfg_for_mcp.tools.mcp_servers.is_empty() {
        let mcp_mgr =
            session::mcp_client::McpClientManager::new(&cfg_for_mcp.tools.mcp_servers).await;
        for warning in mcp_mgr.warnings() {
            eprintln!("MCP warning: {warning}");
            tracing::warn!(warning = %warning, "MCP startup warning");
        }
        let mcp_tool_count = mcp_mgr.tool_count();
        if mcp_tool_count > 0 {
            let mcp_mgr = std::sync::Arc::new(mcp_mgr);
            toolset.add(Box::new(session::toolset::VecToolset::new(
                "mcp",
                session::mcp_tools::all_mcp_tools(mcp_mgr),
            )));
            tracing::info!(count = mcp_tool_count, "MCP tools registered");
        }
    }

    // ── Plugin tools ──
    let cfg_for_plugins = kf_code::shared::read_shared_config(&shared_config).clone();
    let (plugin_registry, plugin_warnings) =
        match session::plugin_tools::load_plugin_registry(&cfg_for_plugins) {
            Ok(rw) => rw,
            Err(e) => {
                eprintln!("Warning: failed to load plugin registry: {e:#}");
                (kf_plugin_host::PluginRegistry::new(), vec![])
            }
        };
    let plugin_tools =
        session::plugin_tools::all_plugin_tools(&plugin_registry, shared_config.clone());
    if !plugin_tools.is_empty() {
        toolset.add(Box::new(session::toolset::VecToolset::new(
            "plugin",
            plugin_tools,
        )));
        tracing::info!(
            count = plugin_registry.active_count(),
            "plugin tools registered"
        );
    }
    for w in plugin_warnings {
        eprintln!("Plugin warning: {w}");
        tracing::warn!(warning = %w, "plugin load warning");
    }

    // ── Stratum in-process tools (feature-gated) ──
    // When the `stratum` feature is enabled, the five core Stratum tools
    // (run, apply, mode, rules, config_validate) are registered as direct
    // Rust calls instead of shell-plugin subprocesses.
    #[cfg(feature = "stratum")]
    {
        let cfg = kf_code::shared::read_shared_config(&shared_config);
        if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum") {
            let stratum_tool_list = session::stratum::stratum_tools();
            let count = stratum_tool_list.len();
            toolset.add(Box::new(session::toolset::VecToolset::new(
                "stratum",
                stratum_tool_list,
            )));
            tracing::info!(count, "stratum in-process tools registered");
        }
    }

    // ── Draw in-process tool (feature-gated) ──
    // When the `draw` feature is enabled, the draw_render tool loads and
    // renders .td.json files using kf_draw_core directly, eliminating
    // the subprocess overhead of shelling out to the kfd binary.
    #[cfg(feature = "draw")]
    {
        let draw_tool_list = session::draw::draw_tools();
        let count = draw_tool_list.len();
        toolset.add(Box::new(session::toolset::VecToolset::new(
            "draw",
            draw_tool_list,
        )));
        tracing::info!(count, "draw in-process tools registered");
    }

    // ── Video in-process tools (feature-gated) ──
    // When the `video` feature is enabled, the eight video tools call
    // kf_video directly, eliminating subprocess overhead.
    #[cfg(feature = "video")]
    {
        let video_tool_list = session::video::video_tools();
        let count = video_tool_list.len();
        toolset.add(Box::new(session::toolset::VecToolset::new(
            "video",
            video_tool_list,
        )));
        tracing::info!(count, "video in-process tools registered");
    }

    // ── Budget in-process tools (feature-gated) ──
    // When the `budget` feature is enabled, the 7 budget tools
    // are registered as direct Rust calls instead of shell-plugin
    // subprocesses. ADR-047 pins this decision.
    #[cfg(feature = "budget")]
    {
        let cfg = kf_code::shared::read_shared_config(&shared_config);
        if cfg
            .tools
            .enabled_plugins
            .iter()
            .any(|n| n == "kf-plugin-sdk3")
        {
            let budget_tool_list = session::budget::all_budget_tools();
            let count = budget_tool_list.len();
            toolset.add(Box::new(session::toolset::VecToolset::new(
                "budget",
                budget_tool_list,
            )));
            tracing::info!(count, "budget in-process tools registered");
        }
    }

    if let Some(sys) = &system {
        // Wired into the executor's PromptBuilder before the first turn
        // (see tui::run_tui and run_non_interactive). Kept as an info
        // log so operators can confirm the override took effect.
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    // If stdout is not a real terminal (piped, redirected, detached pty),
    // the TUI cannot render. Fall back to the same line-mode loop that
    // --non-interactive uses, but read from stdin instead of a pre-baked
    // prompt list so the user can still chat.
    let use_tui = !no_tui && !non_interactive && !no_color && std::io::stdout().is_terminal();
    if use_tui {
        tui::run_tui(
            shared_config,
            adapter,
            toolset,
            (conversation, open_outcome),
            system,
            undo_stack,
            &plugin_registry,
            context_index,
            trace_recorder,
        )
        .await
    } else {
        run_line_mode(
            shared_config,
            adapter,
            toolset,
            (conversation, open_outcome),
            system,
            output,
            max_turns,
            non_interactive,
            no_color,
            &plugin_registry,
            session_id.to_string(),
            context_index,
            trace_recorder,
        )
        .await
    }
}

/// Print a hint listing recent sessions when running non-interactively
/// without an explicit resume target.
fn print_recent_sessions_hint(sessions: &[kf_code::session::session_index::SessionEntry]) {
    eprintln!("Recent sessions (run with --auto-resume or --attach <id> to resume):");
    for (i, e) in sessions
        .iter()
        .enumerate()
        .take(kf_code::daemon::RECENT_SESSIONS_LIMIT)
    {
        eprintln!(
            "  {}. {} — {} messages — {}",
            i + 1,
            e.id,
            e.message_count,
            e.started_at
        );
    }
}
