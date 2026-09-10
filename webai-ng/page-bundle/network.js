/* network.js
 *
 * Network monitoring utilities used by jcode-authored scripts (task 4.6).
 * Coalesces fetch and XMLHttpRequest traffic into events the bridge can
 * forward back to the agent runtime.
 */

(function () {
    'use strict';

    function post(method, data) {
        if (window.__webkitBridgePost) {
            window.__webkitBridgePost(method, data);
        }
    }

    var originalFetch = window.fetch;
    window.fetch = function (input, init) {
        var url = typeof input === 'string' ? input : input.url;
        var method = (init && init.method) || 'GET';
        var requestId = 'fetch_' + Date.now() + '_' + Math.random().toString(36).slice(2, 9);
        post('network.request.start', {
            id: requestId,
            url: url,
            method: method,
            timestamp: Date.now()
        });
        return originalFetch.apply(this, arguments).then(function (response) {
            post('network.request.complete', {
                id: requestId,
                status: response.status,
                statusText: response.statusText,
                timestamp: Date.now()
            });
            return response;
        }, function (error) {
            post('network.request.error', {
                id: requestId,
                error: String(error && error.message || error),
                timestamp: Date.now()
            });
            throw error;
        });
    };

    var OriginalOpen = XMLHttpRequest.prototype.open;
    var OriginalSend = XMLHttpRequest.prototype.send;

    XMLHttpRequest.prototype.open = function (method, url) {
        this.__webkitAiId = 'xhr_' + Date.now() + '_' + Math.random().toString(36).slice(2, 9);
        this.__webkitAiMethod = method;
        this.__webkitAiUrl = url;
        return OriginalOpen.apply(this, arguments);
    };

    XMLHttpRequest.prototype.send = function (body) {
        var id = this.__webkitAiId;
        post('network.request.start', {
            id: id,
            url: this.__webkitAiUrl,
            method: this.__webkitAiMethod,
            timestamp: Date.now()
        });
        this.addEventListener('load', function () {
            post('network.request.complete', {
                id: id,
                status: this.status,
                statusText: this.statusText,
                timestamp: Date.now()
            });
        });
        this.addEventListener('error', function () {
            post('network.request.error', {
                id: id,
                error: 'XMLHttpRequest error',
                timestamp: Date.now()
            });
        });
        return OriginalSend.apply(this, arguments);
    };

    window.WebkitAiNetwork = { installed: true };
})();