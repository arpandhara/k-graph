; Go structural definitions.
;
; The package clause scopes every declaration but is a sibling of them, so it
; is supplied by `GoSyntax::base_scope`, not by this query.
;
; `type_spec` covers structs, interfaces, and aliases; `GoSyntax::refine_kind`
; looks at the `type` field and selects the real `NodeKind`.
;
; Top-level const/var declarations are anchored to `source_file` so local
; variables inside function bodies are not emitted as graph nodes.

(function_declaration
  name: (identifier) @name) @def.function

(method_declaration
  name: (field_identifier) @name) @def.method

(type_spec
  name: (type_identifier) @name) @def.type_alias

(type_alias
  name: (type_identifier) @name) @def.type_alias

(field_declaration
  name: (field_identifier) @name) @def.field

(method_elem
  name: (field_identifier) @name) @def.method

; The `name` field is a `commaSep1`, and the query engine only yields the first
; identifier for `const_spec` when the field is named. Matching direct
; `identifier` children enumerates every name in `const A, B = ...`.
(source_file
  (const_declaration
    (const_spec (identifier) @name)) @def.constant)

(source_file
  (var_declaration
    (var_spec (identifier) @name)) @def.global_variable)
