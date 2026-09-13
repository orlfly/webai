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
    // Resident mode for RSS sampling (#70 Blocker-1): after the prompt
    // completes, stay idle N seconds so the sampler can measure steady-state
    // memory. 0 (default) = exit immediately.
    let mut resident_secs: u64 = 0;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--serve" => mode = LaunchMode::Serve,
            "--headless" => mode = LaunchMode::Headless,
            "--public" => public = true,
            "--prompt" => {
                prompt = it.next().map(String::from);
            }
            "--resident-secs" => {
                resident_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| parse_err("--resident-secs expects a number"))
                    .unwrap_or(0);
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

    match run(
        mode,
        public,
        config_dir.clone(),
        resume,
        prompt,
        resident_secs,
    ) {
        Ok(()) => {}
        Err(e) => {
            // Structured, human-readable startup error (no bare "unknown error").
            eprintln!("webai: {e}");
            // Cold-start remedy: point at the file to create, only when a
            // required config file is absent (PRODUCT-DESIGN §7).
            if matches!(
                e,
                RuntimeError::MissingConfig { .. } | RuntimeError::ConfigParse { .. }
            ) {
                let dir = config_dir
                    .as_deref()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| {
                        webai_config::resolve_config_dir(
                            std::env::var("WEBAI_CONFIG").ok().as_deref(),
                        )
                    });
                eprintln!(
                    "hint: create {}/agent.toml (at minimum `llm = \"<profile>\"`)\
                     \n      and {}/llm.toml (a `[<profile>]` table with model/base_url/endpoint/api_key);\
                     \n      or pass --config-dir <dir>",
                    dir.display(),
                    dir.display()
                );
            }
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
         --resident-secs <n> stay idle n seconds after the prompt (RSS sampling)\n  \
         -V, --version    print version\n  \
         -h, --help       this help",
        env!("CARGO_PKG_VERSION")
    );
}

/// All assembly delegated to the runtime module; the binary has no business
/// logic beyond handing the frontend entry points to the runtime (thin-binary
/// rule; layering `agent < {acp, tui} < bins/webai`).
fn parse_err(msg: &str) -> RuntimeError {
    RuntimeError::Io(format!("invalid argument: {msg}"))
}

fn run(
    mode: LaunchMode,
    public: bool,
    config_dir: Option<PathBuf>,
    resume: Option<PathBuf>,
    prompt: Option<String>,
    resident_secs: u64,
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
            // TUI: run the whole orchestration inside ONE tokio runtime so
            // `tokio::spawn` in SessionBackend has a reactor context (the
            // pre-fix panic "there is no reactor running" came from spawning
            // before the runtime existed).
            let tui_rt =
                tokio::runtime::Runtime::new().map_err(|e| RuntimeError::Io(e.to_string()))?;
            tui_rt.block_on(async {
                webai_tui::run::install_panic_hook();
                let mut guard = webai_tui::run::TerminalGuard::enter()
                    .map_err(|e| RuntimeError::Io(format!("terminal: {e}")))?;

                let session = webai_agent::AgentSession::new(
                    "local",
                    runtime::build_agent_loop(rt),
                    webai_agent::memory_store(rt),
                );
                // Real prompt path: the M4 orchestration driver with the same
                // honest stub executor as headless (script composition drives
                // the browser verbs; the FFI backend attaches via features).
                let handler: Arc<dyn webai_tui::session::PromptHandler> = Arc::new(
                    webai_tui::session::RunnerPromptHandlerShared(Arc::new(
                        webai_tui::session::RunnerPromptHandler::new(
                            Arc::clone(&rt.llm),
                            (*rt.memory).clone(),
                            Arc::new(webai_agent::runner::StubExecutor::default()),
                        ),
                    )),
                );
                let mut backend = webai_tui::session::SessionBackend::try_spawn(
                    std::sync::Arc::new(session),
                    handler,
                )
                .map_err(|e| RuntimeError::Io(e.to_string()))?;

                // Relay SessionEvents -> UiEvents for the run loop, and forward
                // run-loop commands -> backend (send_prompt / shutdown).
                let (ui_tx, ui_rx) = tokio::sync::mpsc::channel::<webai_tui::UiEvent>(
                    webai_tui::session::EVENT_CHANNEL_CAPACITY,
                );
                // Relay task owns the backend: select over SessionEvents (map
                // to UiEvents) and forwarded commands (Send / Shutdown) since
                // SessionBackend's command sender is private.
                let (hold_tx, mut hold_rx) = tokio::sync::mpsc::unbounded_channel();
                let relay = tokio::spawn(async move {
                    loop {
                        // SessionEvent side.
                        tokio::select! {
                            ev = backend.events().recv() => {
                                let Some(ev) = ev else { break };
                                let ui = match ev {
                                    webai_tui::SessionEvent::Step { step } => {
                                        if let Some(b64) = &step.image {
                                            if ui_tx
                                                .send(webai_tui::UiEvent::Image(b64.clone()))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        webai_tui::UiEvent::Delta(match step.observation {
                                            Some(obs) => format!("[{}] {}", step.tool_name, obs),
                                            None => format!("[{}]", step.tool_name),
                                        })
                                    }
                                    webai_tui::SessionEvent::Done { state } => {
                                        webai_tui::UiEvent::Finished(
                                            state.message.unwrap_or_else(|| state.status),
                                        )
                                    }
                                    webai_tui::SessionEvent::Error { message } => {
                                        webai_tui::UiEvent::Finished(format!("error: {message}"))
                                    }
                                };
                                if ui_tx.send(ui).await.is_err() { break; }
                            }
                            cmd = hold_rx.recv() => {
                                let Some(cmd) = cmd else { break };
                                match cmd {
                                    webai_tui::UiCommand::Send { text } => { backend.send_prompt(text); }
                                    webai_tui::UiCommand::Shutdown => break,
                                }
                            }
                        }
                    }
                    backend // hand ownership back for close()
                });
                let res = webai_tui::run::run_real(ui_rx, hold_tx).await;
                let _ = guard.restore();
                // Drain cleanly: dropping the run-loop's command sender closes
                // the relay's command side; the loop then breaks, returns the
                // backend, and we shut it down and await its exit.
                if let Ok(b) = relay.await {
                    let _ = b.shutdown();
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        b.close(),
                    ).await;
                }
                // Non-fatal: interactive exit paths (Ctrl+C / Esc) end Ok.
                match res {
                    Ok(()) => Ok::<(), RuntimeError>(()),
                    Err(e) => Err(RuntimeError::Io(format!("tui: {e}"))),
                }
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
        LaunchOutcome::Headless => {
            if resident_secs > 0 {
                // Idle for the sampler: no work, just resident memory.
                std::thread::sleep(std::time::Duration::from_secs(resident_secs));
            }
            println!("webai: headless mode finished");
        }
    }
    Ok(())
}
