/**
 * Type declarations for the page-bundle building blocks.
 *
 * These describe the globals installed by the document-start scripts in
 * `BUNDLE_SCRIPT_ORDER`. They are for authoring/type-checking only — the
 * runtime is WebKit's JavaScriptCore (定论三: JS 引擎唯一), so there is no
 * Node/QuickJS runtime dependency. Type-checking is an optional dev-time
 * step (see page-bundle/README.md).
 */

/** The bridge client installed by `bridge-client.js`. */
interface WebkitBridge {
  version: string;
  /** Post a serialised observation to the Rust host. */
  emit(serialised: string): void;
  /** Subscribe to a bridge method. */
  subscribe(method: string, handler: (payload: unknown) => void): void;
}

interface Window {
  /** Installed by bridge-client.js. */
  __webkitBridge: WebkitBridge;
  /** Set true by the Rust host once the real bridge is attached. */
  __webkitBridgeReady?: boolean;
  /** Helper for composed scripts to record observations. */
  __webkitBridgePost?: (method: string, data?: unknown) => void;
  /** True once bridge-client.js has loaded. */
  __webkitBridgeLoaded?: boolean;
  /** Buffered observations until the real bridge attaches. */
  __webkitBridgeQueue?: unknown[];
}

/** DOM helpers installed by `dom.js`. */
interface WebkitAiDom {
  find(selector: string): Element | null;
  findAll(selector: string): Element[];
  getVisibleText(): string;
  getVisibleHtml(): string;
}

/** Selector generation installed by `selector.js`. */
interface WebkitAiSelector {
  generate(el: Element): string;
}

/** Accessibility helpers installed by `accessibility/index.js`. */
interface WebkitAiAccessibility {
  tree(): unknown;
}

/** Parser helpers installed by `parser/index.js`. */
interface WebkitAiParser {
  parse(html: string): unknown;
}

/** Action helpers installed by `actions/*.js`. */
interface WebkitAiActions {
  navigate(url: string): void;
  goBack(): void;
  goForward(): void;
  reload(): void;
  click(selector: string): void;
  fill(selector: string, value: string): void;
  select(selector: string, value: string): void;
  hover(selector: string): void;
  drag(source: string, target: string): void;
  pressKey(key: string): void;
  getVisibleText(): string;
  getVisibleHtml(): string;
  screenshot(): void;
}

interface Window {
  WebkitAiDom: WebkitAiDom;
  WebkitAiSelector: WebkitAiSelector;
  WebkitAiAccessibility: WebkitAiAccessibility;
  WebkitAiParser: WebkitAiParser;
  WebkitAiActions: WebkitAiActions;
}
