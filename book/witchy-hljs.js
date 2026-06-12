// Register a highlight.js grammar for witchy and (re-)highlight the book's
// ```witchy code blocks. mdBook bundles highlight.js but doesn't know witchy, so
// without this the blocks render as plain text. Added via `additional-js`.
(function () {
  function defineWitchy(hljs) {
    const KEYWORDS = {
      keyword:
        "fn let var if else match for in while return break continue type " +
        "trait impl actor on import pub inout sink own move spawn where as " +
        "gen yield retain without",
      literal: "true false",
      built_in:
        // capability ops + common builtins, so calls read clearly
        "print read write exists is_dir list subdir make_dir connect listen " +
        "accept send_line send_bytes recv_line recv_all recv_bytes close " +
        "restrict now get_env send length at push concat dict_new insert " +
        "get_or has remove update keys values pairs size " +
        "int_to_string string_length char_count index_of split string_chars " +
        "replace substring to_upper to_lower trim starts_with ends_with " +
        "contains int_to_float float_to_int int_to_duration duration_to_int " +
        "sqrt string_to_int fail",
    };

    const SUBST = {
      className: "subst",
      begin: /\$\{/,
      end: /\}/,
      keywords: KEYWORDS,
    };
    const STRING = {
      className: "string",
      begin: /"/,
      end: /"/,
      contains: [{ begin: /\\./ }, SUBST],
    };
    // `${...}` can contain nested strings and numbers.
    SUBST.contains = [STRING, { className: "number", begin: /\b\d+/ }];

    return {
      name: "witchy",
      case_insensitive: false,
      keywords: KEYWORDS,
      contains: [
        hljs.COMMENT("//", "$"),
        hljs.COMMENT("/\\*", "\\*/"),
        STRING,
        // Numbers, including duration literals (30s, 250ms, 2hr). Longer suffixes
        // first so `ms`/`hr` win over `m`/`h`.
        {
          className: "number",
          begin: /\b\d+(\.\d+)?(ms|hr|s|m|h|d|w)?\b/,
          relevance: 0,
        },
        // Title-case identifiers are types and constructors (Int, String, Dir,
        // Console, Option, Some, Circle, ...).
        { className: "type", begin: /\b[A-Z][A-Za-z0-9_]*\b/, relevance: 0 },
      ],
    };
  }

  function apply() {
    const hljs = window.hljs;
    if (!hljs) return;
    if (!hljs.getLanguage("witchy")) {
      hljs.registerLanguage("witchy", defineWitchy);
    }
    document.querySelectorAll("pre code.language-witchy").forEach(function (el) {
      // Reset to raw text (mdBook may have left a failed/plain highlight) and
      // re-run via the language-witchy class.
      el.textContent = el.textContent;
      el.removeAttribute("data-highlighted");
      el.classList.remove("hljs");
      if (hljs.highlightElement) hljs.highlightElement(el);
      else if (hljs.highlightBlock) hljs.highlightBlock(el);
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", apply);
  } else {
    apply();
  }
})();
