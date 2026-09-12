//! Before/after comparison harness (deliverable record for task #68).
//!
//! Measures compose-call counts with and without the cache, the cache hit
//! rate, and screenshot captures with and without throttling, then asserts
//! the M-2 / §12.6 acceptance bounds.

use std::time::{Duration, Instant};

use webai_bridge::{ComposeCache, ScreenshotDecision, ScreenshotThrottle};
use webai_protocol::{BrowserToolRequest, BrowserVerb};

fn navigate(i: usize) -> BrowserToolRequest {
    BrowserToolRequest {
        verb: BrowserVerb::Navigate,
        args: serde_json::json!({ "url": format!("https://site{}.com", i % 10) }),
    }
}

#[test]
fn perf_before_after_comparison() {
    let steps = 200usize;
    let reqs: Vec<BrowserToolRequest> = (0..steps).map(navigate).collect();

    // BEFORE: compose on every step (no cache).
    let before_start = Instant::now();
    let mut before_compose = 0usize;
    for r in &reqs {
        let _ = webai_script::compose(r).unwrap();
        before_compose += 1;
    }
    let before_elapsed = before_start.elapsed();

    // AFTER: cached — 10 distinct URLs => 10 misses, 190 hits.
    let cache = ComposeCache::default();
    let after_start = Instant::now();
    for r in &reqs {
        let _ = cache.compose(r).unwrap();
    }
    let after_elapsed = after_start.elapsed();

    // Screenshot throttle: 200 mutating steps at min 50ms interval.
    let throttle = ScreenshotThrottle::new(Duration::from_millis(50));
    let mut shots = 0usize;
    for r in &reqs {
        if throttle.decide(&r.verb, true) == ScreenshotDecision::Capture {
            shots += 1;
        }
    }

    // Measured record (printed for the delivery notes).
    println!("== task #68 before/after ==");
    println!(
        "compose calls: before={} after-misses={} after-hits={}",
        before_compose,
        cache.misses(),
        cache.hits()
    );
    println!("compose hit rate: {}%", cache.hit_rate_percent());
    println!(
        "compose elapsed: before={:?} after={:?}",
        before_elapsed, after_elapsed
    );
    println!(
        "screenshots: before={} (one per step) after-captured={} throttled={}",
        steps,
        throttle.captured(),
        throttle.throttled()
    );

    // Acceptance bounds.
    assert_eq!(cache.misses(), 10, "10 distinct urls => exactly 10 misses");
    assert_eq!(cache.hits(), steps as u64 - 10);
    assert!(cache.hit_rate_percent() >= 90, "hit rate must be dominant");
    assert!(
        shots * 2 <= steps,
        "throttle must cut screenshot count substantially (got {shots})"
    );
}
