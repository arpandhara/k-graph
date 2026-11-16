; Rust structural definitions.
;
; Every definition pattern exposes a `@name` capture and a `@def.<kind>`
; capture. The kind suffix is the stable `NodeKind` wire name, so the generic
; executor can build nodes without language-specific match arms.
;
; `impl` blocks are not nodes; they contribute a scope name through
; `@impl.target` so methods land in `Index::load`-style URIs.

(function_item
  name: (identifier) @name) @def.function

(function_signature_item
  name: (identifier) @name) @def.function

(struct_item
  name: (type_identifier) @name) @def.struct

(enum_item
  name: (type_identifier) @name) @def.enum

(enum_variant
  name: (identifier) @name) @def.enum_variant

(trait_item
  name: (type_identifier) @name) @def.trait

(type_item
  name: (type_identifier) @name) @def.type_alias

(mod_item
  name: (identifier) @name) @def.module

(const_item
  name: (identifier) @name) @def.constant

(static_item
  name: (identifier) @name) @def.global_variable

(macro_definition
  name: (identifier) @name) @def.macro

(field_declaration
  name: (field_identifier) @name) @def.field

(impl_item
  type: (_) @impl.target) @impl.scope
