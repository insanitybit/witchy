#include "tree_sitter/parser.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// External scanner for witchy's off-side rule. It emits block-structure tokens
// from indentation:
//   INDENT — a line more indented than the current block opens a new one
//   DEDENT — a line less indented closes one (a multi-level outdent emits one
//            DEDENT per call until the target column is reached)
// Most newlines are whitespace. Before a leading unary `*`, `&`, or `-` at
// statement indentation, STATEMENT_BREAK prevents the preceding expression from
// consuming it as a binary operator. It is not valid inside bracketed expressions.
// A structural
// token is only produced after an actual line break, so an inline `:`/`->` body
// (same line) never opens a block. Blank lines and `//`-comment-only lines never
// affect indentation.

enum TokenType { INDENT, DEDENT, STATEMENT_BREAK };

typedef struct {
    uint32_t len;
    uint32_t cap;
    uint16_t *items;
    int32_t target; // remaining outdent target while emitting multiple DEDENTs, else -1
} Scanner;

static void push(Scanner *s, uint16_t v) {
    if (s->len == s->cap) {
        s->cap = s->cap ? s->cap * 2 : 8;
        s->items = realloc(s->items, s->cap * sizeof(uint16_t));
    }
    s->items[s->len++] = v;
}

static uint16_t top(Scanner *s) { return s->len ? s->items[s->len - 1] : 0; }

void *tree_sitter_witchy_external_scanner_create(void) {
    Scanner *s = calloc(1, sizeof(Scanner));
    s->target = -1;
    push(s, 0);
    return s;
}

void tree_sitter_witchy_external_scanner_destroy(void *payload) {
    Scanner *s = payload;
    free(s->items);
    free(s);
}

unsigned tree_sitter_witchy_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *s = payload;
    unsigned n = 0;
    memcpy(buffer + n, &s->target, sizeof(int32_t));
    n += sizeof(int32_t);
    for (uint32_t i = 0; i < s->len; i++) {
        if (n + sizeof(uint16_t) > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) break;
        memcpy(buffer + n, &s->items[i], sizeof(uint16_t));
        n += sizeof(uint16_t);
    }
    return n;
}

void tree_sitter_witchy_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *s = payload;
    s->len = 0;
    s->target = -1;
    unsigned i = 0;
    if (length >= sizeof(int32_t)) {
        memcpy(&s->target, buffer, sizeof(int32_t));
        i = sizeof(int32_t);
    }
    for (; i + sizeof(uint16_t) <= length; i += sizeof(uint16_t)) {
        uint16_t v;
        memcpy(&v, buffer + i, sizeof(uint16_t));
        push(s, v);
    }
    if (s->len == 0) push(s, 0);
}

static void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

bool tree_sitter_witchy_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *s = payload;
    if (!valid_symbols[INDENT] && !valid_symbols[DEDENT] && !valid_symbols[STATEMENT_BREAK]) return false;

    // Continue a multi-level outdent that began on a previous (zero-width) call.
    if (s->target >= 0) {
        if (valid_symbols[DEDENT] && top(s) > (uint16_t)s->target) {
            s->len--;
            if (top(s) <= (uint16_t)s->target) s->target = -1;
            lexer->result_symbol = DEDENT;
            return true;
        }
        s->target = -1;
        return false;
    }

    bool saw_newline = false;
    bool at_eof = false;
    uint32_t indent = 0;

    for (;;) {
        if (lexer->lookahead == '\n') {
            saw_newline = true;
            indent = 0;
            skip(lexer);
        } else if (lexer->lookahead == '\r') {
            skip(lexer);
        } else if (lexer->lookahead == ' ') {
            indent++;
            skip(lexer);
        } else if (lexer->lookahead == '\t') {
            indent += 8;
            skip(lexer);
        } else if (lexer->lookahead == '/') {
            while (lexer->lookahead != '\n' && lexer->lookahead != 0) skip(lexer);
            indent = 0;
        } else if (lexer->lookahead == 0) {
            at_eof = true;
            break;
        } else {
            break;
        }
    }

    lexer->mark_end(lexer);

    if (at_eof) {
        if (valid_symbols[DEDENT] && top(s) > 0) {
            s->len--;
            lexer->result_symbol = DEDENT;
            return true;
        }
        return false;
    }

    // Only a real line break can change block structure; an inline `: expr`
    // (same line) must not open a block.
    if (!saw_newline) return false;

    uint16_t current = top(s);
    if (indent == current && valid_symbols[STATEMENT_BREAK]
        && (lexer->lookahead == '*' || lexer->lookahead == '&' || lexer->lookahead == '-')) {
        int32_t prefix = lexer->lookahead;
        lexer->advance(lexer, false);
        // These are distinct infix/assignment tokens, not leading unary
        // operators. mark_end still precedes the operator, so an emitted break
        // does not consume it and the ordinary lexer reads the full token.
        if (lexer->lookahead != '=' && !(prefix == '&' && lexer->lookahead == '&')
            && !(prefix == '-' && lexer->lookahead == '>')) {
            lexer->result_symbol = STATEMENT_BREAK;
            return true;
        }
        return false;
    }
    if (indent > current) {
        if (valid_symbols[INDENT]) {
            push(s, (uint16_t)indent);
            lexer->result_symbol = INDENT;
            return true;
        }
    } else if (indent < current) {
        if (valid_symbols[DEDENT]) {
            s->len--;
            if (top(s) > indent) s->target = (int32_t)indent; // more DEDENTs to come
            lexer->result_symbol = DEDENT;
            return true;
        }
    }
    return false;
}
