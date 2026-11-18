//! Configuration parsing for `voxix serve` (SDK plan §3.1: bind defaults,
//! token disclosure modes, isolated data dirs).

use crate::cli::TokenStdoutMode;
use crate::runtime_context::{default_app_data_dir, expand_data_dir};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Default loopback bind (SDK plan §3.1).
pub const DEFAULT_BIND: &str = "127.0.0.1:47832";
/// Protocol version advertised by this build.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub token_stdout: TokenStdoutMode,
    pub max_audio_seconds: Option<u64>,
    pub embed: bool,
    /// Opt-in Opus codec for the v1 audio plane (SDK plan §6; default off).
    pub opus: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMode {
    /// Visible `serve` daemon: persisted bind, discovery document, locks, and
    /// hash-token storage for LAN/desktop use.
    Standalone,
    /// `--embed` child: ephemeral loopback bind, no discovery/locks, stdout
    /// ready-line, stdin lifetime lease.
    Embedded,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub mode: ServiceMode,
    pub bind: String,
    pub data_dir: PathBuf,
    pub token_stdout: TokenStdoutMode,
    pub max_sessions: u32,
    pub max_audio_seconds: u64,
    pub session_resume_window_ms: u64,
    /// Opt-in Opus codec for the v1 audio plane (SDK plan §6; spec default
    /// off — `false` keeps the mandatory PCM baseline).
    pub opus_enabled: bool,
}

impl ServiceConfig {
    /// Resolve the effective data directory: `--data-dir`, then portable
    /// mode, then the default per-OS app-data layout.
    pub fn data_dir() -> Result<PathBuf, String> {
        if let Some(run_dir) = crate::portable::data_dir() {
            return Ok(run_dir.clone());
        }
        default_app_data_dir()
    }

    /// `state/` directory for service-owned files (locks, discovery,
    /// secrets). Created on demand.
    pub fn state_dir(&self) -> Result<PathBuf, String> {
        let dir = self.data_dir.join("state");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create state dir {}: {e}", dir.display()))?;
        Ok(dir)
    }
}

impl ServiceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceMode::Standalone => "standalone",
            ServiceMode::Embedded => "embedded",
        }
    }
}

pub fn from_options(options: ServeOptions) -> Result<ServiceConfig, String> {
    let mode = if options.embed {
        ServiceMode::Embedded
    } else {
        ServiceMode::Standalone
    };

    let bind = match (&mode, &options.bind) {
        (ServiceMode::Embedded, _) => "127.0.0.1:0".to_string(),
        (ServiceMode::Standalone, Some(b)) => b.clone(),
        (ServiceMode::Standalone, None) => DEFAULT_BIND.to_string(),
    };
    // Fail fast on an unparseable bind.
    bind.parse::<SocketAddr>()
        .map_err(|e| format!("invalid --bind '{bind}': {e}"))?;

    let data_dir = expand_data_dir(options.data_dir.as_deref())
        .map(Ok)
        .unwrap_or_else(ServiceConfig::data_dir)?;
    if mode == ServiceMode::Embedded {
        // The embedded child intentionally shares the caller's app data (it
        // runs against the hosting process's managers), but must take no
        // instance/engine locks and write no discovery doc.
        let cfg = ServiceConfig {
            mode,
            bind,
            data_dir,
            token_stdout: TokenStdoutMode::Never,
            max_sessions: 1,
            max_audio_seconds: options.max_audio_seconds.unwrap_or(1800),
            session_resume_window_ms: 30_000,
            opus_enabled: false,
        };
        return Ok(cfg);
    }

    Ok(ServiceConfig {
        mode,
        bind,
        data_dir,
        token_stdout: options.token_stdout,
        max_sessions: 8,
        max_audio_seconds: options.max_audio_seconds.unwrap_or(1800),
        session_resume_window_ms: 30_000,
        opus_enabled: options.opus,
    })
}
