(function() {
    var documentClone = document.cloneNode(true);
    var article = new Readability(documentClone).parse();
    if (!article || !article.content) return false;
    // Replace page body with just the article content for AX tree extraction
    document.body.innerHTML = article.content;
    return true;
})()
