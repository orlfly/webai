/* selector.js
 *
 * Selector-generation utilities used by jcode-authored scripts. Replaces
 * the equivalent logic from the original `web-agent/script/aiagent.js`
 * with a strategy-based implementation that prefers ID > role > uncommon
 * tag > name > distinct classes > href/src > nth-child (task 4.5).
 *
 * Public API:
 *   - WebkitAiSelector.generate(element)
 *   - WebkitAiSelector.getXPath(element)
 *   - WebkitAiSelector.test(selector, context?)
 */

(function () {
    'use strict';

    var COMMON_NODES = ['div', 'span', 'p', 'b', 'i', 'u', 'strong', 'em', 'h2', 'h3'];

    function isCommonTag(tag) {
        return COMMON_NODES.indexOf(tag) !== -1;
    }

    function escapeIdentifier(value) {
        return String(value).replace(/(["\\])/g, '\\$1');
    }

    function nthChildString(element) {
        var parent = element.parentElement;
        if (!parent) {
            return '';
        }
        var siblings = Array.prototype.slice.call(parent.children);
        var matchingSiblings = siblings.some(function (sibling) {
            return sibling !== element && sibling.tagName === element.tagName;
        });
        if (!matchingSiblings) {
            return '';
        }
        var index = siblings.indexOf(element) + 1;
        return ':nth-child(' + index + ')';
    }

    function idSelector(element) {
        if (!element.id) {
            return null;
        }
        var selector = '#' + escapeIdentifier(element.id);
        if (document.querySelectorAll(selector).length === 1) {
            return selector;
        }
        return null;
    }

    function roleSelector(element) {
        var role = element.getAttribute('role');
        if (role) {
            return '[role="' + escapeIdentifier(role) + '"]';
        }
        return null;
    }

    function uncommonTagSelector(element) {
        var tag = element.tagName.toLowerCase();
        if (isCommonTag(tag)) {
            return null;
        }
        if (element.tagName === 'INPUT' && element.hasAttribute('type')) {
            return tag + '[type="' + element.type + '"]';
        }
        return tag;
    }

    function nameSelector(element) {
        if (!element.hasAttribute('id') && element.name) {
            return '[name="' + escapeIdentifier(element.name) + '"]';
        }
        return null;
    }

    function distinctClassSelector(element) {
        if (!element.classList || element.classList.length === 0) {
            return null;
        }
        var siblings = Array.prototype.slice.call(
            (element.parentElement && element.parentElement.children) || []
        );
        var distinct = Array.prototype.filter.call(
            element.classList,
            function (cls) {
                return !siblings.some(function (sibling) {
                    return sibling !== element && sibling.classList.contains(cls);
                });
            }
        );
        if (distinct.length === 0 || distinct.length > 3) {
            return null;
        }
        return '.' + distinct.map(escapeIdentifier).join('.');
    }

    function fileRefSelector(element) {
        var attr = element.hasAttribute('href')
            ? 'href'
            : (element.hasAttribute('src') ? 'src' : null);
        if (!attr) {
            return null;
        }
        var value = element.getAttribute(attr);
        if (!value || value.length > 80) {
            return null;
        }
        var lastSlash = value.lastIndexOf('/');
        var end = lastSlash === -1 ? value : value.substring(lastSlash + 1);
        if (!end) {
            return null;
        }
        return '[' + attr + '$="' + end.replace(/"/g, '\\"') + '"]';
    }

    function commonTagSelector(element) {
        var tag = element.tagName.toLowerCase();
        if (isCommonTag(tag)) {
            return tag;
        }
        return null;
    }

    var STRATEGIES = [
        idSelector,
        roleSelector,
        uncommonTagSelector,
        nameSelector,
        distinctClassSelector,
        fileRefSelector,
        commonTagSelector
    ];

    function uniqueSelector(element) {
        for (var i = 0; i < STRATEGIES.length; i++) {
            var partial = STRATEGIES[i](element);
            if (!partial) {
                continue;
            }
            var withNth = partial + nthChildString(element);
            try {
                if (document.querySelectorAll(withNth).length === 1) {
                    return withNth;
                }
            } catch (err) {
                // Selector with bad characters — skip and try the next strategy.
            }
        }
        // Fall back to XPath-style nth-of-type selector.
        return element.tagName.toLowerCase() + nthChildString(element);
    }

    function generate(element) {
        if (!element || !(element instanceof Element)) {
            return '';
        }
        return uniqueSelector(element);
    }

    function getXPath(element) {
        if (!element || !(element instanceof Element)) {
            return '';
        }
        if (element.id) {
            return '//*[@id="' + element.id.replace(/"/g, '\\"') + '"]';
        }
        var parts = [];
        var current = element;
        while (current && current.nodeType === Node.ELEMENT_NODE) {
            var tag = current.tagName.toLowerCase();
            var parent = current.parentElement;
            if (parent) {
                var siblings = Array.prototype.filter.call(
                    parent.children,
                    function (sibling) { return sibling.tagName === current.tagName; }
                );
                var index = siblings.indexOf(current) + 1;
                parts.unshift(tag + '[' + index + ']');
            } else {
                parts.unshift(tag);
            }
            current = current.parentElement;
        }
        return '/' + parts.join('/');
    }

    function test(selector, context) {
        try {
            return !!(context || document).querySelector(selector);
        } catch (err) {
            return false;
        }
    }

    window.WebkitAiSelector = {
        generate: generate,
        getXPath: getXPath,
        test: test
    };
})();