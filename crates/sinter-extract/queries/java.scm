; Java extraction — capture contract in language.rs. Data, not code.

(class_declaration name: (identifier) @name) @def.class
(record_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(annotation_type_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @def.field
(enum_constant name: (identifier) @name) @def.constant

; extends/implements reference the supertype; `implements` additionally
; records interface-impl pairing (dynamic-dispatch fan-out, like Rust traits)
; superclass also embeds: inherited members resolve on the subclass
; (same member-promotion machinery as Go embedded structs)
(class_declaration superclass: (superclass [
  (type_identifier) @ref.use @embed
  (generic_type (type_identifier) @ref.use @embed)
]))
(class_declaration
  interfaces: (super_interfaces (type_list [
    (type_identifier) @ref.use @trait
    (generic_type (type_identifier) @ref.use @trait)
  ]))) @trait.impl
(interface_declaration
  (extends_interfaces (type_list [
    (type_identifier) @ref.use
    (generic_type (type_identifier) @ref.use)
  ])))

; imports: plain and static share one shape; `.*` is a glob of the module
((import_declaration (scoped_identifier) @import) @_decl
  (#not-match? @_decl "\\*"))
(import_declaration (scoped_identifier) @import.module (asterisk) @import.star)

; calls: bare, and qualified through simple receivers. @refpath is the whole
; invocation; java_absolutize cuts the argument list at the first `(`.
(method_invocation !object name: (identifier) @ref.call)
((method_invocation
  object: [(identifier) (field_access) (this) (super)]
  name: (identifier) @ref.call) @refpath)
; ponytail: chained/exotic receivers enumerated as name-only calls; extend
; the list if a receiver kind goes missing in a golden delta.
(method_invocation
  object: [(method_invocation) (object_creation_expression)
           (parenthesized_expression) (array_access) (string_literal)]
  name: (identifier) @ref.call)

; `new Foo()` calls Foo (instantiation is a call, D14)
(object_creation_expression type: (type_identifier) @ref.call)
(object_creation_expression type: (generic_type (type_identifier) @ref.call))

; local bindings that shadow outer names; a type_identifier type feeds the
; typed-local resolution tier
(local_variable_declaration
  type: (type_identifier) @local.type
  declarator: (variable_declarator name: (identifier) @local))
(local_variable_declaration
  type: [(integral_type) (floating_point_type) (boolean_type)
         (array_type) (generic_type) (scoped_type_identifier)]
  declarator: (variable_declarator name: (identifier) @local))
(formal_parameter
  type: (type_identifier) @local.type
  name: (identifier) @local)
(formal_parameter
  type: [(integral_type) (floating_point_type) (boolean_type)
         (array_type) (generic_type) (scoped_type_identifier)]
  name: (identifier) @local)
(catch_formal_parameter name: (identifier) @local)
(enhanced_for_statement name: (identifier) @local)
