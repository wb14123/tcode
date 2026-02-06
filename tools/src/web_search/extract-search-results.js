(function() {
    var results = [];
    var mainResults = document.querySelectorAll('div._0_SRI.search-result');

    mainResults.forEach(function(el) {
        var titleLink = el.querySelector('a.__sri_title_link');
        if (!titleLink) return;

        var title = titleLink.textContent.trim();
        var url = titleLink.getAttribute('href') || '';

        var snippetEl = el.querySelector('div._0_DESC > div');
        var snippet = '';
        if (snippetEl) {
            // Clone to remove time spans before extracting text
            var clone = snippetEl.cloneNode(true);
            var timeSpans = clone.querySelectorAll('span.__sri-time');
            timeSpans.forEach(function(s) { s.remove(); });
            snippet = clone.textContent.trim();
        }

        var subResults = [];
        // Sub-results are siblings in the parent sri-group, not children of _0_SRI
        var parentGroup = el.closest('div.sri-group');
        if (parentGroup) {
            var subItems = parentGroup.querySelectorAll('div.sr-group div.__srgi');
            subItems.forEach(function(sub) {
                var subTitleEl = sub.querySelector('h3.__srgi-title a._0_URL') ||
                                sub.querySelector('h3 a._0_URL');
                if (!subTitleEl) return;

                var subTitle = subTitleEl.textContent.trim();
                var subUrl = subTitleEl.getAttribute('href') || '';

                var subSnippetEl = sub.querySelector('div.__sri-desc');
                var subSnippet = subSnippetEl ? subSnippetEl.textContent.trim() : '';

                subResults.push({
                    title: subTitle,
                    url: subUrl,
                    snippet: subSnippet
                });
            });
        }

        results.push({
            title: title,
            url: url,
            snippet: snippet,
            sub_results: subResults
        });
    });

    return JSON.stringify(results);
})()
