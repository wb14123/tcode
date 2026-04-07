module.exports = grammar({
  name: 'tcode',
  externals: $ => [$.separator, $.content],
  rules: {
    document: $ => repeat($.block),
    block: $ => seq($.separator, optional($.content)),
  },
});
