(function() {
    var documentClone = document.cloneNode(true);
    var article = new Readability(documentClone).parse();
    if (!article || !article.content) return null;

    // Post-process: strip unnecessary <div> and <span> wrappers to save tokens
    var container = document.createElement('div');
    container.innerHTML = article.content;

    var INLINE_TAGS = ['A','ABBR','B','BDO','BR','CITE','CODE','DFN','EM','I',
        'IMG','KBD','MARK','Q','S','SAMP','SMALL','SPAN','STRONG','SUB','SUP',
        'TIME','U','VAR','WBR'];

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
        var children = Array.from(root.childNodes);
        for (var i = 0; i < children.length; i++) {
            if (children[i].nodeType === 1) clean(children[i]);
        }
        if (root.nodeType !== 1) return;
        var tag = root.tagName;

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

    Array.from(container.children).forEach(clean);
    return container.innerHTML;
})()
