/* actions/screenshot.js
 *
 * Screenshot helper used by jcode-authored scripts (task 5.5). The page
 * cannot directly produce a PNG, so this script returns the document
 * dimensions and a CSS-rendered preview that the host translates into
 * PNG via the WPE backend.
 */

(function () {
    'use strict';

    function capture(name) {
        var element = document.documentElement;
        var rect = element.getBoundingClientRect();
        return {
            ok: true,
            name: name,
            width: Math.ceil(rect.width),
            height: Math.ceil(rect.height),
            // The host renders the actual PNG; the page only signals that
            // a screenshot was requested.
            requested: true
        };
    }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.screenshot = capture;
})();