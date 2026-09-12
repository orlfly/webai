/* actions/composite.js
 *
 * Composed helpers that mix selectors with the network interceptor
 * (task 5.7). `iframeClick`, `customUserAgent`, `expectResponse`, and
 * `assertResponse` are all expressed as scripts.
 */

(function () {
    'use strict';

    function customUserAgent(userAgent) {
        Object.defineProperty(navigator, 'userAgent', { value: userAgent, configurable: true });
        return { ok: true, userAgent: navigator.userAgent };
    }

    function expectResponse(matcher, timeout) {
        return new Promise(function (resolve, reject) {
            var deadline = Date.now() + (timeout || 5000);
            var listener = function (event) {
                if (typeof matcher === 'string' && event.url.indexOf(matcher) === -1) {
                    return;
                }
                if (matcher instanceof RegExp && !matcher.test(event.url)) {
                    return;
                }
                window.removeEventListener('webkit-ai-response', listener);
                resolve(event);
            };
            window.addEventListener('webkit-ai-response', listener);
            setTimeout(function () {
                window.removeEventListener('webkit-ai-response', listener);
                reject(new Error('expectResponse timed out'));
            }, Math.max(deadline - Date.now(), 0));
        });
    }

    function assertResponse(status) {
        return function (event) {
            if (event.status !== status) {
                throw new Error('assertResponse expected ' + status + ', got ' + event.status);
            }
            return { ok: true, status: event.status };
        };
    }

    window.WebkitAiActions = window.WebkitAiActions || {};
    window.WebkitAiActions.customUserAgent = customUserAgent;
    window.WebkitAiActions.expectResponse = expectResponse;
    window.WebkitAiActions.assertResponse = assertResponse;
})();