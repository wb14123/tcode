#include "generated/tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>

enum TokenType {
  SEPARATOR,
  CONTENT,
};

void *tree_sitter_tcode_external_scanner_create(void) {
  return NULL;
}

void tree_sitter_tcode_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_tcode_external_scanner_serialize(void *payload,
                                                      char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_tcode_external_scanner_deserialize(void *payload,
                                                    const char *buffer,
                                                    unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_tcode_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  (void)payload;

  bool valid_separator = valid_symbols[SEPARATOR];
  bool valid_content = valid_symbols[CONTENT];

  if (valid_separator && lexer->get_column(lexer) == 0 &&
      lexer->lookahead == 0x25BA /* ► */) {
    // Consume the entire separator line
    while (!lexer->eof(lexer)) {
      int32_t ch = lexer->lookahead;
      lexer->advance(lexer, false);
      if (ch == '\n') {
        break;
      }
    }
    lexer->mark_end(lexer);
    lexer->result_symbol = SEPARATOR;
    return true;
  }

  if (valid_content) {
    bool consumed = false;
    for (;;) {
      if (lexer->eof(lexer)) {
        if (consumed) {
          lexer->mark_end(lexer);
          lexer->result_symbol = CONTENT;
          return true;
        }
        return false;
      }

      int32_t ch = lexer->lookahead;
      lexer->advance(lexer, false);
      consumed = true;

      if (ch == '\n') {
        // After newline, check if next char is ► at column 0
        if (!lexer->eof(lexer) && lexer->lookahead == 0x25BA &&
            lexer->get_column(lexer) == 0) {
          lexer->mark_end(lexer);
          lexer->result_symbol = CONTENT;
          return true;
        }
      }
    }
  }

  return false;
}
