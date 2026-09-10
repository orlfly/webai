/* accessibility.js
 *
 * Accessibility tree builder. Mirrors the legacy
 * `web-agent/script/accessibility/index.js` and exposes the helpers
 * `computeAccessibleName`, `computeAccessibleDescription`, `getRole`,
 * `isDisabled` as a single building block (task 4.2).
 *
 * The vendored stub uses simple heuristics: tag-name to role mapping and
 * label-lookup. The real implementation would wrap `dom-accessibility-api`.
 */

(function () {
    'use strict';

    var ROLE_BY_TAG = {
        a: 'link',
        button: 'button',
        input: 'textbox',
        textarea: 'textbox',
        select: 'combobox',
        option: 'option',
        nav: 'navigation',
        header: 'banner',
        footer: 'contentinfo',
        main: 'main',
        article: 'article',
        section: 'region',
        aside: 'complementary',
        h1: 'heading',
        h2: 'heading',
        h3: 'heading',
        img: 'img',
        table: 'table'
    };

    function getRole(node) {
        if (!node || !(node instanceof Element)) {
            return 'generic';
        }
        if (node.hasAttribute('role')) {
            return node.getAttribute('role');
        }
        return ROLE_BY_TAG[node.tagName.toLowerCase()] || 'generic';
    }

    function findLabel(node) {
        if (node.id) {
            return document.querySelector('label[for="' + node.id + '"]');
        }
        var parent = node.parentElement;
        while (parent) {
            if (parent.tagName === 'LABEL') {
                return parent;
            }
            parent = parent.parentElement;
        }
        return null;
    }

    function computeAccessibleName(node) {
        if (!node || !(node instanceof Element)) {
            return '';
        }
        if (node.hasAttribute('aria-label')) {
            return node.getAttribute('aria-label');
        }
        if (node.hasAttribute('aria-labelledby')) {
            var id = node.getAttribute('aria-labelledby').split(' ')[0];
            var label = document.getElementById(id);
            if (label) {
                return (label.textContent || '').trim();
            }
        }
        if (node.tagName === 'IMG' && node.alt) {
            return node.alt;
        }
        if (node.tagName === 'INPUT' || node.tagName === 'TEXTAREA') {
            var label = findLabel(node);
            if (label) {
                return (label.textContent || '').trim();
            }
            return node.placeholder || '';
        }
        if (node.tagName === 'BUTTON' || node.tagName === 'A') {
            return (node.textContent || '').trim();
        }
        return '';
    }

    function computeAccessibleDescription(node) {
        if (!node || !(node instanceof Element)) {
            return '';
        }
        if (node.hasAttribute('aria-describedby')) {
            var id = node.getAttribute('aria-describedby').split(' ')[0];
            var element = document.getElementById(id);
            if (element) {
                return (element.textContent || '').trim();
            }
        }
        return '';
    }

    function isDisabled(node) {
        if (!node || !(node instanceof Element)) {
            return false;
        }
        if (node.hasAttribute('disabled')) {
            return true;
        }
        var fieldset = node.closest ? node.closest('fieldset[disabled]') : null;
        return !!fieldset;
    }

    window.WebkitAiAccessibility = {
        getRole: getRole,
        computeAccessibleName: computeAccessibleName,
        computeAccessibleDescription: computeAccessibleDescription,
        isDisabled: isDisabled
    };
})();