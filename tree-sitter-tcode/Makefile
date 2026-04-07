# Build the tree-sitter tcode parser as a shared library.
# The .so/.dylib is loaded by Neovim at runtime via vim.treesitter.language.add().

UNAME := $(shell uname -s)
ifeq ($(UNAME),Darwin)
  LIB = libtree-sitter-tcode.dylib
  SHARED_FLAG = -dynamiclib
else
  LIB = libtree-sitter-tcode.so
  SHARED_FLAG = -shared
endif

GENERATED = src/generated/parser.c
SOURCES = $(GENERATED) src/scanner.c
CFLAGS ?= -O2 -fPIC -I src/generated

.PHONY: all clean generate

all: $(LIB)

$(LIB): $(SOURCES)
	$(CC) $(SHARED_FLAG) $(CFLAGS) -o $@ $(SOURCES)

# Generate parser.c from grammar.js (requires tree-sitter CLI).
# Only needed on fresh clone or after grammar.js changes.
$(GENERATED): grammar.js
	@mkdir -p src/generated
	tree-sitter generate -o src/generated grammar.js

generate: grammar.js
	@mkdir -p src/generated
	tree-sitter generate -o src/generated grammar.js

clean:
	rm -f $(LIB)
	rm -rf src/generated
