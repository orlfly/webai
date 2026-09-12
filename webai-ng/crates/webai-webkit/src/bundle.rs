//! Embedded page-bundle (ARCHITECTURE.md §10 / §4.6).
//!
//! Every page-side module is compiled into the binary via `include_str!` so a
//! deployed single executable has no external script dependency. The injection
//! order is [`BUNDLE_SCRIPT_ORDER`] (order-sensitive, §4.6); `concat_bundle()`
//! preserves it exactly and `self_check()` lets a deployed binary verify the
//! bundle is intact even when the source tree is absent.

use crate::BUNDLE_SCRIPT_ORDER;

/// Embedded sources, keyed exactly as in [`BUNDLE_SCRIPT_ORDER`].
pub const BUNDLE_SOURCES: &[(&str, &str)] = &[
    (
        "bridge-client.js",
        include_str!("../page-bundle/bridge-client.js"),
    ),
    (
        "parser/index.js",
        include_str!("../page-bundle/parser/index.js"),
    ),
    (
        "accessibility/index.js",
        include_str!("../page-bundle/accessibility/index.js"),
    ),
    ("dom.js", include_str!("../page-bundle/dom.js")),
    ("selector.js", include_str!("../page-bundle/selector.js")),
    ("events.js", include_str!("../page-bundle/events.js")),
    ("network.js", include_str!("../page-bundle/network.js")),
    ("storage.js", include_str!("../page-bundle/storage.js")),
    (
        "actions/navigate.js",
        include_str!("../page-bundle/actions/navigate.js"),
    ),
    (
        "actions/history.js",
        include_str!("../page-bundle/actions/history.js"),
    ),
    (
        "actions/interact.js",
        include_str!("../page-bundle/actions/interact.js"),
    ),
    (
        "actions/extract.js",
        include_str!("../page-bundle/actions/extract.js"),
    ),
    (
        "actions/screenshot.js",
        include_str!("../page-bundle/actions/screenshot.js"),
    ),
    (
        "actions/composite.js",
        include_str!("../page-bundle/actions/composite.js"),
    ),
    (
        "legacy/playwright-shim.js",
        include_str!("../page-bundle/legacy/playwright-shim.js"),
    ),
];

/// Verify the embedded set matches `BUNDLE_SCRIPT_ORDER` exactly (same names,
/// same order). Called by `self_check` and unit tests.
pub fn verify_order() -> Result<(), BundleError> {
    if BUNDLE_SOURCES.len() != BUNDLE_SCRIPT_ORDER.len() {
        return Err(BundleError::CountMismatch {
            embedded: BUNDLE_SOURCES.len(),
            declared: BUNDLE_SCRIPT_ORDER.len(),
        });
    }
    for (i, (name, _)) in BUNDLE_SOURCES.iter().enumerate() {
        if *name != BUNDLE_SCRIPT_ORDER[i] {
            return Err(BundleError::OrderMismatch {
                position: i,
                embedded: name,
                declared: BUNDLE_SCRIPT_ORDER[i],
            });
        }
    }
    Ok(())
}

/// Concatenate the bundle in injection order with `;` separators, producing
/// the document-start payload. Order-sensitive (§4.6).
pub fn concat_bundle() -> String {
    let mut out = String::with_capacity(BUNDLE_SOURCES.iter().map(|(_, s)| s.len() + 2).sum());
    for (i, (_, src)) in BUNDLE_SOURCES.iter().enumerate() {
        if i > 0 {
            out.push(';');
            out.push('\n');
        }
        out.push_str(src);
    }
    out
}

/// Deployment self-check: the embedded bundle must be complete and ordered.
/// A deployed binary runs this at startup; it does not touch the filesystem,
/// so deleting the source tree cannot break it.
pub fn self_check() -> Result<BundleReport, BundleError> {
    verify_order()?;
    for (name, src) in BUNDLE_SOURCES {
        if src.trim().is_empty() {
            return Err(BundleError::EmptyModule { name });
        }
    }
    Ok(BundleReport {
        modules: BUNDLE_SOURCES.len(),
        bytes: concat_bundle().len(),
    })
}

/// Deployment self-check summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleReport {
    pub modules: usize,
    pub bytes: usize,
}

/// Structured bundle errors (§7: code + context, never a bare string).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    #[error("embedded bundle has {embedded} modules but BUNDLE_SCRIPT_ORDER declares {declared}")]
    CountMismatch { embedded: usize, declared: usize },
    #[error("bundle order mismatch at position {position}: embedded `{embedded}` vs declared `{declared}`")]
    OrderMismatch {
        position: usize,
        embedded: &'static str,
        declared: &'static str,
    },
    #[error("embedded module `{name}` is empty")]
    EmptyModule { name: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_set_matches_declared_order_exactly() {
        verify_order().unwrap();
        // Spot-check the §4.6 invariants too.
        assert_eq!(BUNDLE_SOURCES.first().unwrap().0, "bridge-client.js");
        assert_eq!(
            BUNDLE_SOURCES.last().unwrap().0,
            "legacy/playwright-shim.js"
        );
        assert!(BUNDLE_SOURCES
            .iter()
            .any(|(n, _)| *n == "actions/screenshot.js"));
    }

    #[test]
    fn every_module_is_non_empty_and_registered() {
        for (name, src) in BUNDLE_SOURCES {
            assert!(!src.trim().is_empty(), "{name} is empty");
            // Each stub registers itself on the __webai namespace.
            assert!(src.contains("__webai"), "{name} does not register");
        }
    }

    #[test]
    fn concat_preserves_injection_order() {
        let joined = concat_bundle();
        let pos_bridge = joined.find("bridge-client").unwrap();
        let pos_dom = joined.find("'dom'").unwrap();
        let pos_shim = joined.find("playwright-shim").unwrap();
        assert!(pos_bridge < pos_dom);
        assert!(pos_dom < pos_shim);
    }

    #[test]
    fn self_check_reports_module_count_and_size() {
        let report = self_check().unwrap();
        assert_eq!(report.modules, BUNDLE_SCRIPT_ORDER.len());
        assert!(report.bytes > 0);
    }

    /// Deployment self-check: the embedded payload does not depend on the
    /// source tree (include_str! baked it in at compile time).
    #[test]
    fn self_check_is_filesystem_independent() {
        // No fs access in this test; success proves independence.
        assert!(self_check().is_ok());
    }
}
