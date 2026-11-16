; Rust call sites and imports.
;
; Calls become `PendingCallSite` records and are resolved in a later pass.
; Imports become normalized `ImportRecord` values.

(call_expression
  function: (_) @call.target) @call.site

(use_declaration) @import.statement
