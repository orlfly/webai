/* dom.js
 *
 * DOM utilities used by scripts the jcode runtime generates. The original
 * `web-agent/script/aiagent.js` carried selector generation, traversal,
 * and visibility helpers; this file is the new home for those helpers,
 * loaded into jcode as a building block (task 4.4).
 *
 * Public API:
 *   - WebkitAiDom.find(selector, context?)
 *   - WebkitAiDom.findAll(selector, context?)
 *   - WebkitAiDom.traverse(root, options?)
 *   - WebkitAiDom.getVisibleText(root?)
 *   - WebkitAiDom.getVisibleHtml(root?)
 *   - WebkitAiDom.isVisible(element)
 *   - WebkitAiDom.cleanHtml(root, options?)
 */

(function () {
    'use strict';

    function isVisible(element) {
        if (!element || !(element instanceof Element)) {
            return false;
        }
        var style = window.getComputedStyle(element);
        if (style.visibility === 'hidden' || parseFloat(style.opacity) === 0) {
            return false;
        }
        var rect = element.getBoundingClientRect();
        return rect.bottom > 0 && rect.right > 0 &&
            rect.top < window.innerHeight && rect.left < window.innerWidth;
    }

    function findAll(selector, context) {
        return Array.prototype.slice.call(
            (context || document).querySelectorAll(selector)
        );
    }

    function find(selector, context) {
        return (context || document).querySelector(selector);
    }

    function traverse(root, options) {
        var opts = options || {};
        var results = [];
        var maxDepth = typeof opts.maxDepth === 'number' ? opts.maxDepth : 100;
        var includeHidden = !!opts.includeHidden;
        var includeText = opts.includeText !== false;

        (function walk(node, depth) {
            if (depth > maxDepth || !node) {
                return;
            }
            if (node.nodeType !== Node.ELEMENT_NODE) {
                return;
            }
            if (!includeHidden && !isVisible(node)) {
                return;
            }
            var rect = node.getBoundingClientRect();
            results.push({
                tagName: node.tagName.toLowerCase(),
                text: includeText ? (node.textContent || '').trim() : '',
                value: node.value || null,
                rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
                visible: isVisible(node),
                attributes: Array.prototype.slice.call(node.attributes).reduce(function (acc, attr) {
                    acc[attr.name] = attr.value;
                    return acc;
                }, {})
            });
            for (var i = 0; i < node.childNodes.length; i++) {
                walk(node.childNodes[i], depth + 1);
            }
        })(root || document.body, 0);

        return results;
    }

    function getVisibleText(root) {
        var element = root || document.body;
        var walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, null, false);
        var buffer = [];
        var node;
        while ((node = walker.nextNode())) {
            var text = (node.nodeValue || '').trim();
            if (text) {
                buffer.push(text);
            }
        }
        return buffer.join('\n');
    }

    function getVisibleHtml(root) {
        var element = root || document.body;
        var clone = element.cloneNode(true);
        var remove = function (node) {
            for (var i = node.childNodes.length - 1; i >= 0; i--) {
                var child = node.childNodes[i];
                if (child.nodeType === Node.COMMENT_NODE) {
                    node.removeChild(child);
                } else if (child.nodeType === Node.ELEMENT_NODE &&
                    (child.tagName === 'SCRIPT' || child.tagName === 'STYLE')) {
                    node.removeChild(child);
                } else if (child.nodeType === Node.ELEMENT_NODE) {
                    remove(child);
                }
            }
        };
        remove(clone);
        return clone.outerHTML;
    }

    function cleanHtml(root, options) {
        var opts = options || {};
        var removeScripts = opts.removeScripts !== false;
        var removeComments = opts.removeComments !== false;
        var clone = root.cloneNode(true);
        (function walk(node) {
            for (var i = node.childNodes.length - 1; i >= 0; i--) {
                var child = node.childNodes[i];
                if (removeScripts && child.nodeType === Node.ELEMENT_NODE &&
                    (child.tagName === 'SCRIPT' || child.tagName === 'STYLE')) {
                    node.removeChild(child);
                    continue;
                }
                if (removeComments && child.nodeType === Node.COMMENT_NODE) {
                    node.removeChild(child);
                    continue;
                }
                if (child.nodeType === Node.ELEMENT_NODE) {
                    walk(child);
                }
            }
        })(clone);
        return clone;
    }

    window.WebkitAiDom = {
        find: find,
        findAll: findAll,
        traverse: traverse,
        getVisibleText: getVisibleText,
        getVisibleHtml: getVisibleHtml,
        isVisible: isVisible,
        cleanHtml: cleanHtml
    };
})();