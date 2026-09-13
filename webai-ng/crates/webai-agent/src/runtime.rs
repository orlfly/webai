//! Runtime assembly (ARCHITECTURE.md §3.3 / §4.9): the thin binary delegates
//! all wiring to this module.
//!
//! Contract:
//! - Config load uses `webai-config`: `agent.toml` / `llm.toml` are fail-fast
//!   (a structured error names the file and key); `embd`/`mem`/`vec` degrade.
//! - Three launch modes: TUI (default), `--serve` (ACP server), `--headless`.
//! - `--public` without pairing credentials is refused at startup (§3.3
//!   network boundary; the pairing handshake itself lands with M6-5).
//! - Session resume: `scan_sessions()` lists persisted JSONL transcripts and
//!   `resume_transcript()` rebuilds one, skipping a trailing truncated line
//!   (FR-5 / M-3).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::AgentLoop;
use webai_config::{ConfigError, LoadedConfig};
use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;

/// A launch mode chosen from CLI flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Default: interactive TUI.
    Tui,
    /// `--serve`: ACP JSON-RPC server.
    Serve,
    /// `--headless`: no UI; loop runs to completion then exits.
    Headless,
}

/// Structured startup error (FR-8 / §7: code + file + detail, no bare strings).
#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeError {
    #[error("missing required config: {file} ({key})")]
    MissingConfig { file: &'static str, key: String },
    #[error("unknown llm profile `{profile}` referenced by agent.toml")]
    UnknownLlmProfile { profile: String },
    #[error("--public requires pairing credentials (FR-8 network boundary)")]
    PublicWithoutPairing,
    #[error("config parse failed: {} ({detail})", path.display())]
    ConfigParse { path: PathBuf, detail: String },
    #[error("no {mode} frontend hook was injected by the binary")]
    MissingHook { mode: &'static str },
    #[error("io error: {0}")]
    Io(String),
}

impl From<ConfigError> for RuntimeError {
    fn from(e: ConfigError) -> Self {
        match e {
            ConfigError::Missing(name) => RuntimeError::MissingConfig {
                file: "config",
                key: name,
            },
            ConfigError::Parse { path, detail } => RuntimeError::ConfigParse { path, detail },
            ConfigError::UnknownLlmProfile(p) => RuntimeError::UnknownLlmProfile { profile: p },
        }
    }
}

/// Assembled services handed to a launch mode.
#[derive(Clone)]
pub struct Runtime {
    pub config: LoadedConfig,
    pub llm: Arc<LlmClient>,
    pub memory: Arc<SharedMemoryStore>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("config_dir", &self.config.config_dir)
            .finish_non_exhaustive()
    }
}

/// Assemble the runtime from a config directory.
pub fn bootstrap(config_dir: &Path) -> Result<Runtime, RuntimeError> {
    let config = webai_config::load_from(config_dir)?;
    // LLM profile validated by the config loader (fail-fast on unknown).
    let llm = Arc::new(LlmClient::with_profile_stub(&config.agent.llm));
    // Memory degrades: no mem.toml / backend -> disabled store (main flow runs).
    // M-2 (Kaneo #50): when embd.toml configures a real embedding backend,
    // wire the BGE-M3 adapter into the vector channel so semantic recall
    // uses real model output; backend absent/failed keeps the placeholder.
    let memory = Arc::new(if config.memory.is_some() {
        let mut store = SharedMemoryStore::new();
        if let Some(embd) = config.embedding.as_ref() {
            if !embd.backend.is_empty() {
                let model = webai_embedding::BgeM3Adapter::new(
                    webai_embedding::BgeM3Config {
                        model: if embd.model.is_empty() {
                            "BAAI/bge-m3".into()
                        } else {
                            embd.model.clone()
                        },
                        endpoint: embd.endpoint.clone(),
                        dim: if embd.dim > 0 { embd.dim } else { 1024 },
                    },
                );
                if !store.set_embedder(Arc::new(model)) {
                    tracing::warn!(
                        "embedding adapter dimension mismatch; vector channel keeps placeholder"
                    );
                }
            }
        }
        store
    } else {
        SharedMemoryStore::disabled()
    });
    Ok(Runtime {
        config,
        llm,
        memory,
    })
}

/// Validate the `--public` gate: pairing credentials must exist when serving
/// publicly (§3.3 / FR-8). Returns `Err(PublicWithoutPairing)` otherwise.
pub fn check_public_gate(public: bool, has_pairing: bool) -> Result<(), RuntimeError> {
    if public && !has_pairing {
        return Err(RuntimeError::PublicWithoutPairing);
    }
    Ok(())
}

/// The result of a launch (kept as data so the thin binary can print it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Tui,
    Serve,
    Headless,
}

/// Frontend entry points injected by the binary (thin-binary rule §3.3, and
/// the §3.2 layering rule `agent < {acp, tui}`: this crate cannot depend on
/// the frontend crates, so the binary hands the entry points in).
/// A frontend entry point handed in by the binary.
pub type LaunchHook = Box<dyn Fn(&Runtime) -> Result<(), RuntimeError>>;

pub struct LaunchHooks {
    /// TUI entry: builds the session backend and runs the App loop.
    pub tui: LaunchHook,
    /// ACP serve entry: starts the JSON-RPC dispatcher transports.
    pub serve: LaunchHook,
}

impl std::fmt::Debug for LaunchHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchHooks").finish_non_exhaustive()
    }
}

/// Launch the runtime in the requested mode. The binary only calls this; all
/// wiring lives in this module (thin-binary rule §3.3).
///
/// - `Tui` / `Serve` delegate to the injected frontend hooks.
/// - `Headless` runs the AgentLoop end to end inside this module: assemble the
///   runner from the runtime services and execute one prompt via the loop
///   driver (`AgentRunner::run`) with a passing-through executor.
pub fn launch(
    rt: &Runtime,
    mode: LaunchMode,
    hooks: Option<&LaunchHooks>,
    prompt: Option<&str>,
) -> Result<LaunchOutcome, RuntimeError> {
    match mode {
        LaunchMode::Tui => {
            let hooks = hooks.ok_or(RuntimeError::MissingHook { mode: "tui" })?;
            (hooks.tui)(rt)?;
            Ok(LaunchOutcome::Tui)
        }
        LaunchMode::Serve => {
            let hooks = hooks.ok_or(RuntimeError::MissingHook { mode: "serve" })?;
            (hooks.serve)(rt)?;
            Ok(LaunchOutcome::Serve)
        }
        LaunchMode::Headless => {
            run_headless(rt, prompt.unwrap_or_default())?;
            Ok(LaunchOutcome::Headless)
        }
    }
}

/// Headless mode: run one prompt through the plan-act-observe driver and print
/// the final observation to stdout. Uses the pass-through executor (headless
/// runs are script/composition driven; the browser tool lands with the FFI
/// backend) so the run is honest about what executed.
fn run_headless(rt: &Runtime, prompt: &str) -> Result<(), RuntimeError> {
    use crate::runner::{AgentRunner, RunConfig, StepOutcome, StubExecutor};

    let loop_config = RunConfig {
        max_steps: 30,
        duplicate_threshold: 2,
        auto_plan_on_multi_step: true,
        script_memory_enabled: true,
    };
    let summariser =
        crate::summariser::HistorySummariser::new(crate::summariser::SummariserConfig::default());
    let runner = AgentRunner::new(loop_config, (*rt.memory).clone(), summariser);
    let exec = StubExecutor::default();
    let runtime = tokio::runtime::Runtime::new().map_err(|e| RuntimeError::Io(e.to_string()))?;
    let (steps, outcome, _plan) =
        runtime.block_on(async { runner.run(prompt, &exec, &rt.llm).await });

    for step in &steps {
        println!(
            "[step] {} -> {}",
            step.tool_name,
            step.observation.as_deref().unwrap_or("")
        );
    }
    match outcome {
        StepOutcome::Done { state, message } => {
            println!("done ({state}): {}", message.unwrap_or_default());
            Ok(())
        }
        StepOutcome::Guard(err) => Err(RuntimeError::Io(format!("guard: {err}"))),
        StepOutcome::Error { code, message } => Err(RuntimeError::Io(format!("{code}: {message}"))),
    }
}

/// Scan `~/.webai/sessions/*.jsonl` (or `dir`) for persisted transcripts.
pub fn scan_sessions(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Rebuild the transcript from a persisted JSONL file (FR-5 / M-3): every
/// line must parse as a transcript record via `transcript_from_jsonl`
/// (schema-validated `{"role", "text"}`); a trailing truncated/invalid line is
/// skipped (single-line loss is the documented crash-recovery bound). An
/// invalid line that is *not* at the end of the file is a structured error.
/// Returns `(session_id, transcript_lines)` with raw JSONL text preserved.
pub fn resume_transcript(path: &Path) -> Result<(String, Vec<String>), RuntimeError> {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let raw = std::fs::read_to_string(path).map_err(|e| RuntimeError::Io(e.to_string()))?;
    let mut lines = Vec::new();
    let total: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    for (i, line) in total.iter().enumerate() {
        match crate::session::transcript_from_jsonl(line) {
            Some(_) => lines.push((*line).to_string()),
            None => {
                let is_last = i + 1 == total.len();
                if is_last {
                    // Truncated trailing record: tolerated (M-3 bound).
                    continue;
                }
                return Err(RuntimeError::ConfigParse {
                    path: path.to_path_buf(),
                    detail: format!("invalid transcript record at line {}", i + 1),
                });
            }
        }
    }
    Ok((session_id, lines))
}

/// The runtime's shared memory store (borrowed view for frontend assembly).
pub fn memory_store(rt: &Runtime) -> Arc<SharedMemoryStore> {
    Arc::clone(&rt.memory)
}

/// The assembled `AgentLoop` for a runtime (thin helper the frontends share).
pub fn build_agent_loop(rt: &Runtime) -> Arc<AgentLoop> {
    Arc::new(AgentLoop::new(
        Arc::clone(&rt.llm),
        Arc::clone(&rt.memory),
        vec![],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, content).unwrap();
    }

    fn config_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("webai-rt-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn agent_toml() -> &'static str {
        "llm = \"cloud\"\nmemory = \"default\"\nmax_steps = 30\nduplicate_threshold = 2\n"
    }

    fn llm_toml() -> &'static str {
        "[cloud]\nmodel = \"test-model\"\nbase_url = \"http://localhost\"\n"
    }

    #[test]
    fn bootstrap_fail_fast_on_missing_agent_toml() {
        let dir = config_dir("noffast");
        // Only llm.toml present.
        write(&dir.join("llm.toml"), llm_toml());
        let err = bootstrap(&dir).unwrap_err();
        assert!(
            matches!(err, RuntimeError::MissingConfig { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn bootstrap_fail_fast_on_missing_llm_toml() {
        let dir = config_dir("nollm");
        write(&dir.join("agent.toml"), agent_toml());
        let err = bootstrap(&dir).unwrap_err();
        assert!(matches!(err, RuntimeError::MissingConfig { .. }));
    }

    #[test]
    fn bootstrap_fail_fast_on_unknown_llm_profile() {
        let dir = config_dir("badprofile");
        write(&dir.join("agent.toml"), "llm = \"nope\"\n");
        write(&dir.join("llm.toml"), llm_toml());
        let err = bootstrap(&dir).unwrap_err();
        assert!(
            matches!(err, RuntimeError::UnknownLlmProfile { ref profile } if profile == "nope"),
            "got {err:?}"
        );
    }

    #[test]
    fn bootstrap_degrades_without_optional_files() {
        let dir = config_dir("degrade");
        write(&dir.join("agent.toml"), agent_toml());
        write(&dir.join("llm.toml"), llm_toml());
        let rt = bootstrap(&dir).unwrap();
        // No mem.toml -> memory degraded (disabled store) but present.
        assert!(rt.config.memory.is_none());
        // The runtime is assembled and usable.
        let _loop = build_agent_loop(&rt);
    }

    #[test]
    fn bootstrap_succeeds_with_memory_file_present() {
        let dir = config_dir("mem");
        write(&dir.join("agent.toml"), agent_toml());
        write(&dir.join("llm.toml"), llm_toml());
        write(
            &dir.join("mem.toml"),
            "backend = \"kuzu\"\nsession_dir = \"/tmp\"\n",
        );
        let rt = bootstrap(&dir).unwrap();
        assert!(rt.config.memory.is_some());
    }

    #[test]
    fn public_gate_requires_pairing() {
        assert!(check_public_gate(false, false).is_ok());
        assert!(check_public_gate(true, true).is_ok());
        let err = check_public_gate(true, false).unwrap_err();
        assert!(matches!(err, RuntimeError::PublicWithoutPairing));
    }

    #[test]
    fn scan_sessions_lists_sorted_jsonl() {
        let dir = config_dir("scan");
        write(&dir.join("b.jsonl"), "{}\n");
        write(&dir.join("a.jsonl"), "{}\n");
        write(&dir.join("ignore.txt"), "x");
        let found = scan_sessions(&dir);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.jsonl", "b.jsonl"]);
    }

    #[test]
    fn resume_transcript_skips_truncated_trailing_line() {
        let dir = config_dir("resume");
        let path = dir.join("sess-9.jsonl");
        write(
            &path,
            "{\"role\":\"user\",\"text\":\"hi\"}\n{\"role\":\"assistant\",\"text\":\"yo\"}\n{\"trunc",
        );
        let (id, lines) = resume_transcript(&path).unwrap();
        assert_eq!(id, "sess-9");
        assert_eq!(lines.len(), 2, "truncated trailing line must be skipped");
        assert!(lines[0].contains("\"role\":\"user\""));
    }

    #[test]
    fn resume_transcript_missing_file_is_structured_error() {
        let dir = config_dir("miss");
        let err = resume_transcript(&dir.join("nope.jsonl")).unwrap_err();
        assert!(matches!(err, RuntimeError::Io(_)));
    }

    #[test]
    fn launch_modes_exist() {
        // The three modes are representable (thin binary delegates).
        let modes = [LaunchMode::Tui, LaunchMode::Serve, LaunchMode::Headless];
        assert_eq!(modes.len(), 3);
    }

    /// M-3 crash-recovery fuzz: kill -9 at any byte offset (simulated by
    /// truncating the file at every possible point) must never fail wholesale;
    /// recovery always yields the complete lines only.
    #[test]
    fn crash_recovery_fuzz_truncation_at_any_offset() {
        let dir = config_dir("fuzz");
        let valid = "{\"role\":\"user\",\"text\":\"step-1\"}\n\
                     {\"role\":\"assistant\",\"text\":\"step-2\"}\n\
                     {\"role\":\"user\",\"text\":\"step-3\"}\n";
        let path = dir.join("fuzz-sess.jsonl");
        write(&path, valid);

        // Sanity: full file recovers 3 lines.
        let (_, lines) = resume_transcript(&path).unwrap();
        assert_eq!(lines.len(), 3);

        // Truncate at every byte offset from 1..len; recovery must never
        // panic or error, and must return at most 3 valid lines with no
        // partial JSON.
        for cut in 1..valid.len() {
            let truncated = &valid[..cut];
            write(&path, truncated);
            let (_, lines) = resume_transcript(&path).unwrap();
            assert!(lines.len() <= 3, "cut at {cut} recovered {}", lines.len());
            for l in &lines {
                assert!(
                    serde_json::from_str::<serde_json::Value>(l).is_ok(),
                    "cut at {cut} produced invalid line: {l}"
                );
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// M-3 tightened by task 62: mid-file corruption is a structured
    /// ConfigParse error (schema validation rejects bad records); only a
    /// trailing truncated line is tolerated.
    #[test]
    fn crash_recovery_midfile_corruption_is_structured_error() {
        let dir = config_dir("midcorrupt");
        let path = dir.join("mid.jsonl");
        write(
            &path,
            "{\"role\":\"user\",\"text\":\"a\"}\n\
             GARBAGE-NOT-JSON\n\
             {\"role\":\"assistant\",\"text\":\"c\"}\n",
        );
        let err = resume_transcript(&path).unwrap_err();
        match err {
            RuntimeError::ConfigParse { detail, .. } => {
                assert!(detail.contains("line 2"), "unexpected detail: {detail}")
            }
            other => panic!("expected ConfigParse, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn headless_launch_runs_the_loop_and_completes() {
        let dir = config_dir("headless");
        write(&dir.join("agent.toml"), agent_toml());
        write(&dir.join("llm.toml"), llm_toml());
        let rt = bootstrap(&dir).expect("bootstrap must succeed with a minimal config");

        let outcome = launch(&rt, LaunchMode::Headless, None, Some("open example.com"))
            .expect("headless launch must complete");
        assert_eq!(outcome, LaunchOutcome::Headless);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tui_without_hook_is_structured_missing_hook() {
        let dir = config_dir("nohook");
        write(&dir.join("agent.toml"), agent_toml());
        write(&dir.join("llm.toml"), llm_toml());
        let rt = bootstrap(&dir).unwrap();
        let err = launch(&rt, LaunchMode::Tui, None, None).unwrap_err();
        assert!(matches!(err, RuntimeError::MissingHook { mode: "tui" }));
        let err = launch(&rt, LaunchMode::Serve, None, None).unwrap_err();
        assert!(matches!(err, RuntimeError::MissingHook { mode: "serve" }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_gate_still_refuses_without_pairing() {
        assert!(check_public_gate(true, false).is_err());
        assert!(check_public_gate(false, false).is_ok());
    }

    #[test]
    fn resume_transcript_rejects_invalid_middle_line() {
        let dir = config_dir("resume-invalid");
        let path = dir.join("s1.jsonl");
        std::fs::write(
            &path,
            "{\"role\":\"user\",\"text\":\"hi\"}\nnot-json\n{\"role\":\"assistant\",\"text\":\"ok\"}\n",
        )
        .unwrap();
        let err = resume_transcript(&path).unwrap_err();
        match err {
            RuntimeError::ConfigParse { detail, .. } => assert!(detail.contains("line 2")),
            other => panic!("expected ConfigParse, got {other:?}"),
        }
        // Trailing truncated line is tolerated.
        std::fs::write(
            &path,
            "{\"role\":\"user\",\"text\":\"hi\"}\n{\"role\":\"assistant\",\"text\":\"ok\"}\n{\"role\":\"ass",
        )
        .unwrap();
        let (id, lines) = resume_transcript(&path).unwrap();
        assert_eq!(id, "s1");
        assert_eq!(lines.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = PathBuf::new();
    }
}
