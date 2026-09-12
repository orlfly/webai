//! Fixture-page contract tests (PRODUCT-DESIGN.md §6 M-6, task: fixture CI).
//!
//! The benchmark fixtures must exist in-repo, satisfy their DOM-node
//! contracts and ship with CI. These are pure text/structure checks (no
//! browser required); real-WebKit loading is covered by the e2e suites.

use std::path::{Path, PathBuf};
use std::process::Command;

/// webai-ng workspace root (fixtures/ lives here).
fn webai_ng_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn fixture(name: &str) -> String {
    let path = webai_ng_root().join("fixtures").join("pages").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {} must exist in-repo: {e}", path.display()))
}

fn count_tags(html: &str, tag: &str) -> usize {
    html.matches(&format!("<{tag}")).count()
}

#[test]
fn static_page_has_at_least_100_dom_nodes_and_background_image() {
    let html = fixture("static.html");
    assert!(
        count_tags(&html, "section") >= 20,
        "static fixture must ship its full section table"
    );
    // Node count: open tags + close tags (self-closing none in this fixture).
    let open_tags = html.matches('<').count()
        - html.matches("</").count()
        - html.matches("<!").count()
        - html.matches("<?").count();
    let nodes = open_tags + html.matches("</").count();
    assert!(
        nodes >= 100,
        "static fixture must have >= 100 DOM nodes, got {nodes}"
    );
    assert!(
        html.contains("background-image"),
        "static fixture must include a background image"
    );
}

#[test]
fn spa_page_renders_at_most_200_nodes_and_documents_the_contract() {
    let html = fixture("spa.html");
    assert!(
        html.contains("at most 200 nodes"),
        "spa fixture must document its node-budget contract"
    );
    // Payload cap: the item list is capped at 100 => <= 200 rendered nodes.
    assert!(
        html.contains("slice(0, 100)"),
        "spa render must cap the XHR payload at 100 items"
    );
    let items = fixture("items.json");
    let count = items[1..items.len() - 1].split(',').count();
    assert!(
        count <= 100,
        "items.json must ship at most 100 items, got {count}"
    );
}

#[test]
fn fixtures_are_tracked_by_git_for_ci_distribution() {
    let out = Command::new("git")
        .args(["ls-files", "--", "fixtures/pages"])
        .current_dir(webai_ng_root())
        .output()
        .expect("git must be available in CI");
    let tracked = String::from_utf8_lossy(&out.stdout);
    for name in ["static.html", "spa.html", "items.json"] {
        assert!(
            tracked.contains(name),
            "fixtures/pages/{name} must be git-tracked (got: {tracked})"
        );
    }
}
