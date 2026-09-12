// page-bundle module: selector
// Injected at document-start in BUNDLE_SCRIPT_ORDER (ARCHITECTURE.md 4.6).
// Registered on the webai bridge namespace; the real implementation lands
// with the M2 page-bundle milestone (PR #79).
(function (global) {
  'use strict';
  var ns = (global.__webai = global.__webai || {});
  ns['selector'] = { name: 'selector', ready: true };
})(typeof window !== 'undefined' ? window : globalThis);
