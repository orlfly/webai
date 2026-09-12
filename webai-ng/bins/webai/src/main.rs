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
use std::sync::Arc;

use webai_agent::runtime::{self, LaunchMode, LaunchOutcome, RuntimeError};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut mode = LaunchMode::Tui;
    let mut public = false;
    let mut config_dir: Option<PathBuf> = None;
    let mut resume: Option<PathBuf> = None;
    let mut prompt: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--serve" => mode = LaunchMode::Serve,
            "--headless" => mode = LaunchMode::Headless,
            "--public" => public = true,
            "--prompt" => {
                prompt = it.next().map(String::from);
            }
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

    match run(mode, public, config_dir, resume, prompt) {
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
/// logic beyond handing the frontend entry points to the runtime (thin-binary
/// rule; layering `agent < {acp, tui} < bins/webai`).
fn run(
    mode: LaunchMode,
    public: bool,
    config_dir: Option<PathBuf>,
    resume: Option<PathBuf>,
    prompt: Option<String>,
) -> Result<(), RuntimeError> {
    // --public without pairing credentials is refused at startup.
    runtime::check_public_gate(public, std::env::var_os("WEBAI_PAIRING_KEY").is_some())?;

    let dir = config_dir.unwrap_or_else(|| {
        webai_config::resolve_config_dir(std::env::var("WEBAI_CONFIG").ok().as_deref())
    });
    let rt = runtime::bootstrap(&dir)?;

    // Resume wiring: rebuild a persisted transcript if requested (FR-5). The
    // transcript is validated with `transcript_from_jsonl` inside the runtime
    // (schema-checked records; a trailing truncated line is tolerated).
    if let Some(path) = resume {
        let (session_id, lines) = runtime::resume_transcript(&path)?;
        println!(
            "webai: resuming session `{session_id}` with {} recovered records",
            lines.len()
        );
    }

    let hooks = runtime::LaunchHooks {
        tui: Box::new(|rt| {
            // TUI: assemble the shared AgentSession and run the backend run
            // loop (webai_tui::session::serve). The App is driven from the
            // SessionEvents; terminal rendering lands with M6-2.
            let session = webai_agent::AgentSession::new(
                "local",
                runtime::build_agent_loop(rt),
                webai_agent::memory_store(rt),
            );
            let handler: Arc<dyn webai_tui::session::PromptHandler> =
                Arc::new(webai_tui::session::LoopPromptHandler);
            let backend =
                webai_tui::session::SessionBackend::spawn(std::sync::Arc::new(session), handler);
            let runtime =
                tokio::runtime::Runtime::new().map_err(|e| RuntimeError::Io(e.to_string()))?;
            runtime.block_on(async {
                backend
                    .close()
                    .await
                    .map_err(|e| RuntimeError::Io(e.to_string()))?;
                Ok::<(), RuntimeError>(())
            })?;
            println!("webai: TUI session loop finished");
            Ok(())
        }),
        serve: Box::new(|rt| {
            // Serve: ACP JSON-RPC dispatcher with a per-session registry.
            let _registry = webai_acp::AcpSessionRegistry::new();
            let _loop_ = runtime::build_agent_loop(rt);
            println!("webai: ACP dispatcher assembled; WS transport lands in M6-3");
            Ok(())
        }),
    };

    let outcome = runtime::launch(&rt, mode, Some(&hooks), prompt.as_deref())?;
    match outcome {
        LaunchOutcome::Tui => println!("webai: TUI mode finished"),
        LaunchOutcome::Serve => println!("webai: serve mode finished"),
        LaunchOutcome::Headless => println!("webai: headless mode finished"),
    }
    Ok(())
}
