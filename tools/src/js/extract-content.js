(function() {
    var documentClone = document.cloneNode(true);
    var article = new Readability(documentClone).parse();
    if (article && article.content) {
        return article.content;
    }
    return null;
})()
