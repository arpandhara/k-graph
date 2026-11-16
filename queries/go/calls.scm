; Go call sites and imports.
;
; Every `import_spec` is captured individually, including specs inside a
; parenthesized import block. `GoSyntax::imports` normalizes the path, alias,
; dot import, and blank import.

(call_expression
  function: (_) @call.target) @call.site

(import_spec) @import.statement
