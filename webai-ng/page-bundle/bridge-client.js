/* bridge-client.js
 *
 * Bridge client injected at document-start into every frame. Exposes the
 * `window.__webkitBridge` global so that scripts the runtime composes can
 * post observations back through `bridge.emit`.
 *
 * Loaded into `jcode` as a building block (task 4.3); the runtime wraps
 * each composed per-step script in a harness that calls
 * `__webkitBridge.inject`.
 */

(function () {
    'use strict';

    var listeners = {};
    var pendingInjections = [];

    function emit(method, data) {
        var payload = {
            type: 'event',
            method: method,
            data: data || {}
        };
        if (window.__webkitBridge && typeof window.__webkitBridge.emit === 'function') {
            window.__webkitBridge.emit(JSON.stringify(payload));
            return;
        }
        pendingInjections.push(payload);
    }

    function drainPending() {
        if (!window.__webkitBridge || typeof window.__webkitBridge.emit !== 'function') {
            return;
        }
        for (var i = 0; i < pendingInjections.length; i++) {
            window.__webkitBridge.emit(JSON.stringify(pendingInjections[i]));
        }
        pendingInjections.length = 0;
    }

    window.__webkitBridge = {
        version: '0.1.0',
        emit: function (serialised) {
            // The real implementation is injected by the Rust host after
            // document-start; until then the messages are buffered.
            try {
                var payload = JSON.parse(serialised);
                (window.__webkitBridgeQueue || (window.__webkitBridgeQueue = [])).push(payload);
            } catch (e) {
                // Drop malformed payloads silently so a page error doesn't
                // cascade into the agent loop.
            }
        },
        subscribe: function (method, handler) {
            (listeners[method] || (listeners[method] = [])).push(handler);
        }
    };

    // Re-drain pending observations whenever the real bridge shows up.
    Object.defineProperty(window, '__webkitBridgeReady', {
        configurable: true,
        set: function (value) {
            if (value === true) {
                drainPending();
            }
        }
    });

    // Expose a small helper for composed scripts to record observations.
    window.__webkitBridgePost = emit;

    // Signal that the client is loaded. Page scripts can use this to wait
    // for `window.__webkitBridge` to be safe to call.
    window.__webkitBridgeLoaded = true;
})();