; TypeScript / TSX structural definitions.
;
; `lexical_declaration`/`variable_declaration` capture arrow functions and
; function expressions assigned to a variable, which the TypeScript grammar
; does not model as `function_declaration` nodes.
;
; TypeScript and TSX use the same query text against two grammars.

(function_declaration
  name: (identifier) @name) @def.function

(generator_function_declaration
  name: (identifier) @name) @def.function

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name) @def.method

(abstract_method_signature
  name: [(property_identifier) (private_property_identifier)] @name) @def.method

(method_signature
  name: [(property_identifier) (private_property_identifier)] @name) @def.method

(class_declaration
  name: (type_identifier) @name) @def.class

(abstract_class_declaration
  name: (type_identifier) @name) @def.class

(interface_declaration
  name: (type_identifier) @name) @def.interface

(type_alias_declaration
  name: (type_identifier) @name) @def.type_alias

(enum_declaration
  name: (identifier) @name) @def.enum

(enum_assignment
  name: (property_identifier) @name) @def.enum_variant

(public_field_definition
  name: [(property_identifier) (private_property_identifier)] @name) @def.field

(property_signature
  name: (property_identifier) @name) @def.property

(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @def.function

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @def.function

(internal_module
  name: (identifier) @name) @def.module

(module
  name: (identifier) @name) @def.module
