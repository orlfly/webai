/* actions/history.js
 *
 * Scripts that drive the browser history (task 5.2). Each function returns
 * a JSON-serialisable result that the runtime forwards back as the
 * observation for the injected script.
 */

(function () {
    'use strict';

    function goBack() { history.back(); return { ok: true }; }
    function goForward() { history.forward(); return { ok: true }; }
    function reload() { location.reload(); return { ok: true }; }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.goBack = goBack;
    window.WebkitAiActions.goForward = goForward;
    window.WebkitAiActions.reload = reload;
})();