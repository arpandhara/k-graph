; Python call sites and imports.

(call
  function: (_) @call.target) @call.site

(import_statement) @import.statement

(import_from_statement) @import.statement
