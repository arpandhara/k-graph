; Python structural definitions.
;
; Module-level assignments are captured as global variables; the language
; syntax refines all-uppercase names to constants and methods to constructors.

(function_definition
  name: (identifier) @name) @def.function

(class_definition
  name: (identifier) @name) @def.class

(module
  (expression_statement
    (assignment left: (identifier) @name) @def.global_variable))
