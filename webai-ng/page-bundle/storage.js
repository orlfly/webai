/* storage.js
 *
 * Storage monitoring utilities used by jcode-authored scripts (task 4.6).
 * Posts observation events for localStorage, sessionStorage, and cookie
 * mutations.
 */

(function () {
    'use strict';

    function post(method, data) {
        if (window.__webkitBridgePost) {
            window.__webkitBridgePost(method, data);
        }
    }

    function intercept(storage, type) {
        if (!storage) {
            return;
        }
        var originalSetItem = storage.setItem.bind(storage);
        var originalGetItem = storage.getItem.bind(storage);
        var originalRemoveItem = storage.removeItem.bind(storage);
        var originalClear = storage.clear.bind(storage);

        storage.setItem = function (key, value) {
            originalSetItem(key, value);
            post('storage.set', { type: type, key: key, value: value, timestamp: Date.now() });
        };

        storage.getItem = function (key) {
            var value = originalGetItem(key);
            post('storage.get', { type: type, key: key, value: value, timestamp: Date.now() });
            return value;
        };

        storage.removeItem = function (key) {
            originalRemoveItem(key);
            post('storage.remove', { type: type, key: key, timestamp: Date.now() });
        };

        storage.clear = function () {
            originalClear();
            post('storage.clear', { type: type, timestamp: Date.now() });
        };
    }

    intercept(window.localStorage, 'localStorage');
    intercept(window.sessionStorage, 'sessionStorage');

    window.WebkitAiStorage = { installed: true };
})();