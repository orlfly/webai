/* actions/extract.js
 *
 * Page-content extraction helpers used by jcode-authored scripts (task 5.4
 * and task 5.6).
 */

(function () {
    'use strict';

    function getVisibleText() {
        if (window.WebkitAiDom) {
            return WebkitAiDom.getVisibleText();
        }
        return (document.body.textContent || '').trim();
    }

    function getVisibleHtml() {
        if (window.WebkitAiDom) {
            return WebkitAiDom.getVisibleHtml();
        }
        return document.body.innerHTML;
    }

    function consoleLogs(limit) {
        // Page-side console history is not directly accessible; the host
        // surfaces the last N lines through `bridge.emit`. The runtime
        // forwards them to `bridge.evaluate` callers via the
        // `__webkitAiConsole` global injected by the host.
        var logs = window.__webkitAiConsole || [];
        return logs.slice(-1 * (limit || logs.length));
    }

    function accessibilityTree() {
        if (window.WebkitAiAccessibility) {
            (function walk(node) {
                if (!node || node.nodeType !== 1) { return null; }
                var role = WebkitAiAccessibility.getRole(node);
                var name = WebkitAiAccessibility.computeAccessibleName(node);
                if ((!role || role === 'generic') && !name) {
                    return null;
                }
                return {
                    tag: node.tagName.toLowerCase(),
                    role: role,
                    name: name,
                    children: Array.prototype.slice.call(node.children || [])
                        .map(walk).filter(Boolean)
                };
            })(document.body);
        }
        return { role: 'document', name: document.title, children: [] };
    }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.getVisibleText = getVisibleText;
    window.WebkitAiActions.getVisibleHtml = getVisibleHtml;
    window.WebkitAiActions.consoleLogs = consoleLogs;
    window.WebkitAiActions.accessibilityTree = accessibilityTree;
})();