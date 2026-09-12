/* legacy/playwright-shim.js
 *
 * Compatibility shim that maps the legacy `playwright.*` global to
 * `bridge.inject` so existing call sites keep working for one release
 * (task 4.8). The shim delegates to the runtime by serialising the call
 * into the bridge protocol format; the runtime then composes the real
 * script and pushes it into WebKit via `bridge.inject`.
 *
 * The shim is removed once the deprecation window ends (task 10.3.4).
 *
 * DEPENDENCIES (must be loaded before this script, in BUNDLE_SCRIPT_ORDER):
 *   - WebkitAiDom      (dom.js)      — getVisibleText / getVisibleHtml
 *   - WebkitAiSelector (selector.js) — selector generation
 *   - WebkitAiActions  (actions/*)   — navigate / click / fill / etc.
 *   - window.__webkitBridge (bridge-client.js) — emit / inject
 */

(function () {
    'use strict';

    var SHIM_VERSION = '0.1.0';

    function buildScript(command, args) {
        var selectors = args.selector ? JSON.stringify(args.selector) : 'null';
        var value = args.value ? JSON.stringify(args.value) : 'null';
        var key = args.key ? JSON.stringify(args.key) : 'null';
        var url = args.url ? JSON.stringify(args.url) : 'null';
        var script = args.script ? JSON.stringify(args.script) : 'null';
        var selector2 = args.sourceSelector ? JSON.stringify(args.sourceSelector) : 'null';
        var selector3 = args.targetSelector ? JSON.stringify(args.targetSelector) : 'null';
        var iframe = args.iframeSelector ? JSON.stringify(args.iframeSelector) : 'null';

        switch (command) {
            case 'navigate':
                return 'window.location.href = ' + url + ';';
            case 'click':
                return 'document.querySelector(' + selectors + ').click(); ({ ok: true });';
            case 'iframe_click':
                return 'document.querySelector(' + iframe + ').contentDocument.querySelector(' + selectors + ').click(); ({ ok: true });';
            case 'fill':
                return '(function(){var e=document.querySelector(' + selectors + '); e.value=' + value + '; e.dispatchEvent(new Event("input",{bubbles:true})); e.dispatchEvent(new Event("change",{bubbles:true})); return { ok: true };})();';
            case 'select':
                return 'document.querySelector(' + selectors + ').value = ' + value + '; ({ ok: true });';
            case 'hover':
                return 'document.querySelector(' + selectors + ').dispatchEvent(new MouseEvent("mouseover",{bubbles:true})); ({ ok: true });';
            case 'evaluate':
                return '(' + script + ')';
            case 'drag':
                return '(function(){var s=document.querySelector(' + selector2 + '); var t=document.querySelector(' + selector3 + '); s.dispatchEvent(new DragEvent("dragstart",{bubbles:true})); t.dispatchEvent(new DragEvent("drop",{bubbles:true})); return { ok: true };})();';
            case 'press_key':
                return 'document.querySelector(' + selectors + ').dispatchEvent(new KeyboardEvent("keydown",{key:' + key + '})); ({ ok: true });';
            case 'get_visible_text':
                return 'JSON.stringify(WebkitAiDom.getVisibleText());';
            case 'get_visible_html':
                return 'JSON.stringify(WebkitAiDom.getVisibleHtml());';
            case 'go_back':
                return 'history.back(); ({ ok: true });';
            case 'go_forward':
                return 'history.forward(); ({ ok: true });';
            case 'reload':
                return 'location.reload(); ({ ok: true });';
            default:
                return '({ ok: false, error: "unsupported command: ' + command + '" });';
        }
    }

    function call(command, args) {
        var script = buildScript(command, args || {});
        if (window.__webkitBridge && typeof window.__webkitBridge.inject === 'function') {
            return window.__webkitBridge.inject(script);
        }
        return { ok: false, error: 'bridge not ready' };
    }

    // Legacy `playwright.*` surface. Each method delegates to the bridge.
    window.playwright = {
        version: SHIM_VERSION,
        call: call,
        navigate: function (args) { return call('navigate', args); },
        click: function (args) { return call('click', args); },
        iframe_click: function (args) { return call('iframe_click', args); },
        fill: function (args) { return call('fill', args); },
        select: function (args) { return call('select', args); },
        hover: function (args) { return call('hover', args); },
        drag: function (args) { return call('drag', args); },
        press_key: function (args) { return call('press_key', args); },
        evaluate: function (args) { return call('evaluate', args); },
        get_visible_text: function () { return call('get_visible_text', {}); },
        get_visible_html: function () { return call('get_visible_html', {}); },
        go_back: function () { return call('go_back', {}); },
        go_forward: function () { return call('go_forward', {}); },
        reload: function () { return call('reload', {}); },
        accessibilityTree: function () {
            return call('evaluate', {
                script: 'JSON.stringify((function(){var t=[];function w(n){if(n.nodeType!==1)return null;var r=WebkitAiAccessibility.getRole(n);var nm=WebkitAiAccessibility.computeAccessibleName(n);if(!r||r==="generic")return null;return{tag:n.tagName.toLowerCase(),role:r,name:nm,children:Array.prototype.slice.call(n.children).map(w).filter(Boolean)};}return w(document.body);})())'
            });
        }
    };
})();