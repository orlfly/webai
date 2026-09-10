/* events.js
 *
 * Event monitoring utilities used by jcode-authored scripts. Replaces the
 * capture logic embedded in the original `web-agent/script/aiagent.js`
 * (task 4.6).
 */

(function () {
    'use strict';

    var EVENT_TYPES = ['click', 'input', 'change', 'submit', 'load', 'error', 'resize', 'scroll'];

    function targetInfo(target) {
        if (!target || !(target instanceof Element)) {
            return null;
        }
        return {
            tagName: target.tagName.toLowerCase(),
            id: target.id || null,
            classes: Array.prototype.slice.call(target.classList || []),
            text: (target.textContent || '').trim().substring(0, 200)
        };
    }

    function capture(event, type) {
        if (!window.__webkitBridgePost) {
            return;
        }
        window.__webkitBridgePost('dom.event', {
            type: type,
            target: targetInfo(event.target),
            bubbles: event.bubbles,
            cancelable: event.cancelable,
            timestamp: Date.now()
        });
    }

    EVENT_TYPES.forEach(function (type) {
        document.addEventListener(type, function (event) {
            capture(event, type);
        }, true);
    });

    window.WebkitAiEvents = {
        watched: EVENT_TYPES
    };
})();