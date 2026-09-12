/* actions/interact.js
 *
 * DOM event helpers used by jcode-authored scripts (task 5.3). Click,
 * fill, select, hover, drag, press_key.
 */

(function () {
    'use strict';

    function find(selector) {
        var element = document.querySelector(selector);
        if (!element) {
            throw new Error('element not found: ' + selector);
        }
        return element;
    }

    function click(selector) {
        var element = find(selector);
        element.click();
        return { ok: true, tag: element.tagName.toLowerCase() };
    }

    function iframeClick(iframeSelector, selector) {
        var iframe = document.querySelector(iframeSelector);
        if (!iframe || !iframe.contentDocument) {
            throw new Error('iframe not accessible: ' + iframeSelector);
        }
        var element = iframe.contentDocument.querySelector(selector);
        if (!element) {
            throw new Error('element not found inside iframe: ' + selector);
        }
        element.click();
        return { ok: true, tag: element.tagName.toLowerCase() };
    }

    function fill(selector, value) {
        var element = find(selector);
        element.value = value;
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return { ok: true };
    }

    function select(selector, value) {
        var element = find(selector);
        element.value = value;
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return { ok: true };
    }

    function hover(selector) {
        var element = find(selector);
        element.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
        element.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true }));
        return { ok: true };
    }

    function drag(sourceSelector, targetSelector) {
        var source = find(sourceSelector);
        var target = find(targetSelector);
        source.dispatchEvent(new DragEvent('dragstart', { bubbles: true }));
        target.dispatchEvent(new DragEvent('drop', { bubbles: true }));
        return { ok: true };
    }

    function pressKey(selector, key) {
        var target = selector ? find(selector) : document.body;
        target.dispatchEvent(new KeyboardEvent('keydown', { key: key, bubbles: true }));
        target.dispatchEvent(new KeyboardEvent('keyup', { key: key, bubbles: true }));
        return { ok: true };
    }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.click = click;
    window.WebkitAiActions.iframeClick = iframeClick;
    window.WebkitAiActions.fill = fill;
    window.WebkitAiActions.select = select;
    window.WebkitAiActions.hover = hover;
    window.WebkitAiActions.drag = drag;
    window.WebkitAiActions.pressKey = pressKey;
})();