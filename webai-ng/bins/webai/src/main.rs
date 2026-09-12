//! `webai` binary (ARCHITECTURE.md §3.3): a thin launcher.
//!
//! All wiring lives in `webai_agent::runtime` (thin-binary rule); this file
//! only parses flags and delegates:
//!
//! - default:  interactive TUI
//! - `--serve`: ACP JSON-RPC server
//! - `--headless`: no UI
//! - `--public`: requires pairing credentials (refused otherwise, FR-8)
//! - `--config-dir <dir>`: explicit config directory (tests / sandboxing)
//! - `--resume <session.jsonl>`: resume a persisted transcript (FR-5)

use std::path::PathBuf;

use webai_agent::runtime::{self, LaunchMode, LaunchOutcome, RuntimeError};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut mode = LaunchMode::Tui;
    let mut public = false;
    let mut config_dir: Option<PathBuf> = None;
    let mut resume: Option<PathBuf> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--serve" => mode = LaunchMode::Serve,
            "--headless" => mode = LaunchMode::Headless,
            "--public" => public = true,
            "--resume" => {
                resume = it.next().map(PathBuf::from);
            }
            "--config-dir" => {
                config_dir = it.next().map(PathBuf::from);
            }
            "--version" | "-V" => {
                println!("webai {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => {
                eprintln!("unknown flag: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    match run(mode, public, config_dir, resume) {
        Ok(()) => {}
        Err(e) => {
            // Structured, human-readable startup error (no bare "unknown error").
            eprintln!("webai: {e}");
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!(
        "webai {} — AI browser\n\n\
         Usage: webai [flags]\n\n\
         Flags:\n  \
         --serve          start the ACP JSON-RPC server\n  \
         --headless       run without a UI\n  \
         --public         expose the server publicly (requires pairing)\n  \
         --config-dir <d> explicit config directory (default ~/.webai/config)\n  \
         --resume <file>  resume a persisted session transcript\n  \
         -V, --version    print version\n  \
         -h, --help       this help",
        env!("CARGO_PKG_VERSION")
    );
}

/// All assembly delegated to the runtime module; the binary has no business
/// logic (thin-binary rule).
fn run(
    mode: LaunchMode,
    public: bool,
    config_dir: Option<PathBuf>,
    resume: Option<PathBuf>,
) -> Result<(), RuntimeError> {
    // --public without pairing credentials is refused at startup.
    runtime::check_public_gate(public, std::env::var_os("WEBAI_PAIRING_KEY").is_some())?;

    let dir = config_dir.unwrap_or_else(|| {
        webai_config::resolve_config_dir(std::env::var("WEBAI_CONFIG").ok().as_deref())
    });
    let rt = runtime::bootstrap(&dir)?;

    // Resume wiring: rebuild a persisted transcript if requested (FR-5).
    if let Some(path) = resume {
        let (session_id, lines) = runtime::resume_transcript(&path)?;
        println!(
            "webai: resuming session `{session_id}` with {} recovered records",
            lines.len()
        );
    }

    match runtime::launch(&rt, mode) {
        LaunchOutcome::Tui => {
            println!("webai: TUI mode (interactive UI lands in M6)");
        }
        LaunchOutcome::Serve => {
            println!("webai: ACP serve mode (server transport lands in M6)");
        }
        LaunchOutcome::Headless => {
            println!("webai: headless mode");
        }
    }
    Ok(())
}
