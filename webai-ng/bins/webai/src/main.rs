//! webai binary entry point (ARCHITECTURE.md §10).
//!
//! Startup runs the embedded-bundle `self_check()` first: a deployed single
//! executable must verify its document-start payload before any UI mode
//! starts. Failure is a structured exit (§7), never a silent boot.

use webai_webkit::self_check;

fn main() {
    // Deployment self-check (task 67 AC): report the embedded bundle and
    // refuse to start a broken binary.
    match self_check() {
        Ok(report) => {
            eprintln!(
                "webai {} bundle self-check ok: modules={}, bytes={}",
                env!("CARGO_PKG_VERSION"),
                report.modules,
                report.bytes
            );
        }
        Err(err) => {
            // Structured exit: code + context on stderr, non-zero status.
            eprintln!("webai bundle self-check failed: {err}");
            std::process::exit(2);
        }
    }

    println!(
        "webai {} (webai-ng workspace skeleton, M1-1).",
        env!("CARGO_PKG_VERSION")
    );
}
