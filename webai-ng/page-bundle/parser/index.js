/* parser.js
 *
 * Acorn parser bridge. Mirrors the legacy `web-agent/script/acorn/index.js`
 * but exposes the parser as a building block the jcode runtime can
 * compose into scripts (task 4.2).
 *
 * Implementation note: the real parser would load `acorn` directly. The
 * vendored stub uses `Function('return ' + source)()` to validate that the
 * source parses as JavaScript; downstream code can swap in the real acorn
 * once it is bundled.
 */

(function () {
    'use strict';

    function parse(source) {
        try {
            new Function(source);
            return { ok: true, body: source };
        } catch (err) {
            return { ok: false, error: String(err && err.message || err) };
        }
    }

    window.WebkitAiParser = {
        parse: parse,
        version: '0.1.0'
    };
})();