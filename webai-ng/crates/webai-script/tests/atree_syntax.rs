//! Composed sources must be syntactically valid JS (guard against r#"-edit
//! regressions that only surface as SyntaxError on the real device).
//! Kaneo #103.
use webai_protocol::BrowserVerb;

#[test]
fn composed_sources_are_valid_js() {
    let verbs = [
        BrowserVerb::AccessibilityTree,
        BrowserVerb::GetText,
        BrowserVerb::GetHtml,
        BrowserVerb::Evaluate,
        BrowserVerb::Navigate,
        BrowserVerb::Click,
        BrowserVerb::Fill,
        BrowserVerb::Hover,
        BrowserVerb::Drag,
        BrowserVerb::PressKey,
        BrowserVerb::Screenshot,
        BrowserVerb::Snapshot,
        BrowserVerb::Download,
    ];
    for verb in verbs {
        let req = webai_protocol::BrowserToolRequest { verb, args: serde_json::json!({"script":"1","selector":"div","value":"v","url":"http://x/","key":"Enter","source":"#s","target":"#t","text":"t","path":"/tmp/p","data":"d"}) };
        std::fs::create_dir_all("./target/composed").unwrap();
        let m = webai_script::compose(&req).unwrap();
        for (stage, src) in [("exec", &m.execute_src), ("verify", &m.verify_src)] {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/composed").join(format!(
                "composed_{}_{}.js",
                verb.canonical_name(),
                stage
            ));
            // Top-level `return ...` (driver IIFE) is valid in eval contexts;
            // wrap in a function body for --check.
            std::fs::write(&path, format!("function wrapper() {{\n{src}\n}}")).unwrap();
            let st = std::process::Command::new("node")
                .arg("--check")
                .arg(&path)
                .status()
                .expect("node available on PATH");
            assert!(
                st.success(),
                "composed {} [{stage}] is invalid JS:\n{src}",
                verb.canonical_name()
            );
        }
    }
}
