// page-bundle module: dom
// Injected at document-start in BUNDLE_SCRIPT_ORDER (ARCHITECTURE.md 4.6).
// Registered on the webai bridge namespace; the real implementation lands
// with the M2 page-bundle milestone (PR #79).
(function (global) {
  'use strict';
  var ns = (global.__webai = global.__webai || {});
  ns['dom'] = { name: 'dom', ready: true };
})(typeof window !== 'undefined' ? window : globalThis);
