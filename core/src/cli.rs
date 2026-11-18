use clap::Parser;
use std::path::PathBuf;

/// Where the first-run token plaintext is surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum TokenStdoutMode {
    /// Print plaintext to stdout when explicitly requested; otherwise only the
    /// daemon handshake conventions apply (stderr once when interactive).
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(clap::Subcommand, Debug, Clone)]
pub enum Command {
    /// Run the authenticated transcription service: a headless daemon (no
    /// Tauri/GUI/mic/paste init) serving REST +, later, the v1 protocol.
    Serve {
        /// Socket address to bind (e.g. 0.0.0.0:47832). Defaults to
        /// 127.0.0.1:47832; port 0 selects an ephemeral port (printed).
        #[arg(long, value_name = "IP:PORT")]
        bind: Option<String>,

        /// Isolated app-data directory for this instance (tests / advanced).
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,

        /// First-run token disclosure: auto prints plaintext once to stderr
        /// when interactive; always prints to stdout; never prints.
        #[arg(
            long,
            value_enum,
            default_value = "auto",
            value_name = "auto|always|never"
        )]
        token_stdout: TokenStdoutMode,

        /// Per-session audio duration cap in seconds (advertised in config).
        #[arg(long, value_name = "N")]
        max_audio_seconds: Option<u64>,

        /// Opt-in Opus codec for the v1 audio plane (default off; the
        /// service config `opus_enabled` is the persisted projection).
        #[arg(long)]
        opus: bool,

        /// Embedded child launch mode (hidden): binds 127.0.0.1:0, writes one
        /// ready-line JSON to stdout, and treats stdin EOF as the lifetime
        /// lease. No discovery file, no locks, no CORS headers.
        #[arg(long, hide = true)]
        embed: bool,

        /// Test helper (hidden): assert this process hosts interactive-priority
        /// policy and exit 0/1. Used by the no-background-throttling gate.
        #[arg(long, hide = true)]
        check_process_policy: bool,
    },
}

#[derive(Parser, Debug, Clone, Default)]
#[command(name = "voxix", about = "voxix - Speech to Text")]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Start with the main window hidden
    #[arg(long)]
    pub start_hidden: bool,

    /// Disable the system tray icon
    #[arg(long)]
    pub no_tray: bool,

    /// Toggle transcription on/off (sent to running instance)
    #[arg(long)]
    pub toggle_transcription: bool,

    /// Toggle transcription with post-processing on/off (sent to running instance)
    #[arg(long)]
    pub toggle_post_process: bool,

    /// Cancel the current operation (sent to running instance)
    #[arg(long)]
    pub cancel: bool,

    /// Enable debug mode with verbose logging
    #[arg(long)]
    pub debug: bool,

    /// Transcribe this WAV (16 kHz mono) headlessly and exit. Runs the same
    /// batch transcription path as the app — no mic, no VAD, no download
    /// (the model must already be installed).
    #[arg(short = 'f', long, value_name = "WAV")]
    pub transcribe_file: Option<PathBuf>,

    /// Model id to load for --transcribe-file (default: the selected model).
    #[arg(long)]
    pub model: Option<String>,

    /// Hard-select the compute device for --transcribe-file by its registry
    /// index (see --list-devices). Omit to use the persisted accelerator
    /// setting. transcribe-cpp (whisper-family) models only.
    #[arg(long, value_name = "N")]
    pub device_index: Option<usize>,

    /// List the transcribe-cpp compute devices (with indices) and exit.
    #[arg(long)]
    pub list_devices: bool,

    /// List the available models (with ids) and exit. Pass an id to --model.
    /// Honors --json for machine-readable output.
    #[arg(long)]
    pub list_models: bool,

    /// Repeat the transcription N times (best_ms reports the fastest run).
    #[arg(long, value_name = "N")]
    pub repeat: Option<usize>,

    /// Emit --transcribe-file results as JSON.
    #[arg(long)]
    pub json: bool,
}
