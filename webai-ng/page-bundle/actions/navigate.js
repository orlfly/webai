/* actions/navigate.js
 *
 * Script the jcode runtime injects to navigate the current page. The
 * script assigns `window.location.href` and waits for the `load` event
 * before returning. The host then observes a `page.load` notification
 * through `bridge.emit` (task 5.1).
 */

(function () {
    'use strict';

    function navigate(url, waitUntil) {
        return new Promise(function (resolve) {
            var done = function () {
                window.removeEventListener('load', done);
                resolve({ url: window.location.href, title: document.title });
            };
            window.addEventListener('load', done);
            window.location.href = url;
        });
    }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.navigate = navigate;
})();