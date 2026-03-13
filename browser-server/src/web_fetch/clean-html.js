// Shared HTML cleaning function used by both extract-content.js and tests.
// Takes an HTML string, strips LLM-irrelevant content, returns cleaned HTML.
var cleanHtml = function(html) {
    var container = document.createElement('div');
    container.innerHTML = html;

    var INLINE_TAGS = ['A','ABBR','B','BDO','BR','CITE','CODE','DFN','EM','I',
        'IMG','KBD','MARK','Q','S','SAMP','SMALL','SPAN','STRONG','SUB','SUP',
        'TIME','U','VAR','WBR'];

    var STRIP_ATTRS = ['class','id','style','srcset','role',
        'loading','decoding','fetchpriority','crossorigin','referrerpolicy'];
    var IMG_STRIP = ['width','height'];
    var A_STRIP = ['target','rel'];

    function isInline(node) {
        if (node.nodeType !== 1) return true;
        return INLINE_TAGS.indexOf(node.tagName) !== -1;
    }

    function unwrap(el) {
        var parent = el.parentNode;
        while (el.firstChild) parent.insertBefore(el.firstChild, el);
        parent.removeChild(el);
    }

    function clean(root) {
        // Remove HTML comments first
        var children = Array.from(root.childNodes);
        for (var i = 0; i < children.length; i++) {
            var child = children[i];
            if (child.nodeType === 8) { // Comment node
                root.removeChild(child);
                continue;
            }
            if (child.nodeType === 1) clean(child);
        }
        if (root.nodeType !== 1) return;
        var tag = root.tagName.toUpperCase();

        // Remove SVG, noscript, source elements entirely
        if (tag === 'SVG' || tag === 'NOSCRIPT' || tag === 'SOURCE') {
            root.parentNode.removeChild(root);
            return;
        }

        // For <picture>, keep only the <img> fallback, then unwrap
        if (tag === 'PICTURE') {
            var img = root.querySelector('img');
            if (img) {
                root.parentNode.insertBefore(img, root);
            }
            root.parentNode.removeChild(root);
            if (img) clean(img);
            return;
        }

        // Strip data: URIs — remove the img entirely
        if (tag === 'IMG') {
            var src = root.getAttribute('src') || '';
            if (src.indexOf('data:') === 0 || src === '') {
                root.parentNode.removeChild(root);
                return;
            }
        }

        // Strip unwanted attributes
        var toRemove = [];
        for (var j = 0; j < root.attributes.length; j++) {
            var name = root.attributes[j].name;
            if (STRIP_ATTRS.indexOf(name) !== -1) { toRemove.push(name); continue; }
            if (name.indexOf('data-') === 0) { toRemove.push(name); continue; }
            if (name.indexOf('aria-') === 0) { toRemove.push(name); continue; }
            if (tag === 'IMG' && IMG_STRIP.indexOf(name) !== -1) { toRemove.push(name); continue; }
            if (tag === 'A' && A_STRIP.indexOf(name) !== -1) { toRemove.push(name); continue; }
        }
        for (var k = 0; k < toRemove.length; k++) root.removeAttribute(toRemove[k]);

        // Unwrap <span> with no attributes
        if (tag === 'SPAN' && root.attributes.length === 0) {
            unwrap(root);
            return;
        }

        // Unwrap <div> with no attributes when it adds no structure
        if (tag === 'DIV' && root.attributes.length === 0) {
            var kids = Array.from(root.childNodes).filter(function(n) {
                return !(n.nodeType === 3 && n.textContent.trim() === '');
            });
            // Single div child → flatten
            if (kids.length === 1 && kids[0].nodeType === 1 && kids[0].tagName === 'DIV') {
                unwrap(root);
                return;
            }
            // Only inline content → unwrap
            if (kids.length > 0 && kids.every(isInline)) {
                unwrap(root);
                return;
            }
        }
    }

    // Remove top-level comment nodes (container.children skips non-element nodes)
    Array.from(container.childNodes).forEach(function(n) {
        if (n.nodeType === 8) container.removeChild(n);
    });
    Array.from(container.children).forEach(clean);
    return container.innerHTML;
};
