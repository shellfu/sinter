; TypeScript extraction — capture contract in language.rs. Data, not code.

(function_declaration name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
; Class fields initialized with callable values are methods in normal
; TypeScript APIs (`fetch = (...) => ...`), not data fields.
(public_field_definition
  name: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @def.method
(interface_declaration name: (type_identifier) @name) @def.interface
; interface method signatures are declared symbols (Iface::method):
; fan-out sources for implements pairing
(method_signature name: (property_identifier) @name) @def.method
(enum_declaration name: (identifier) @name) @def.enum
(type_alias_declaration name: (type_identifier) @name) @def.typealias

; top-level (possibly exported) bindings only; function-body consts are
; locals, not symbols. Arrow/function values are functions (engine prefers
; the non-variable kind when two patterns claim one node).
(program (lexical_declaration (variable_declarator name: (identifier) @name)) @def.variable)
(program
  (export_statement
    (lexical_declaration (variable_declarator name: (identifier) @name)) @def.variable))
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(arrow_function) (function_expression)])) @def.function)
(program
  (export_statement
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)])) @def.function))

; `implements` pairs the class with the interface (dynamic-dispatch
; fan-out, like Rust traits); `extends` on an interface is inheritance —
; both reference the supertype
(class_declaration
  (class_heritage (implements_clause [
    (type_identifier) @ref.use @trait
    (generic_type name: (type_identifier) @ref.use @trait)
  ]))) @trait.impl
(interface_declaration
  (extends_type_clause type: [
    (type_identifier) @ref.use @trait
    (generic_type name: (type_identifier) @ref.use @trait)
  ])) @trait.impl

; named imports bind the item: module + name joined by the engine
(import_statement
  (import_clause (named_imports (import_specifier !alias name: (identifier) @import.name)))
  source: (string) @import.module)
(import_statement
  (import_clause (named_imports (import_specifier
    name: (identifier) @import.name
    alias: (identifier) @import.alias)))
  source: (string) @import.module)
; default import binds the exported default under the local name
(import_statement
  (import_clause (identifier) @import.name)
  source: (string) @import.module)
(import_statement
  (import_clause (namespace_import (identifier) @import.alias))
  source: (string) @import)
; dynamic import bound to a const acts as a module alias
(variable_declarator
  name: (identifier) @import.alias
  value: (await_expression
    (call_expression function: (import) arguments: (arguments (string) @import))))
; barrel re-exports are imports of this file for chain walking
(export_statement
  (export_clause (export_specifier name: (identifier) @import.name))
  source: (string) @import.module)

; `export * from './x'` forwards every top-level name of x;
; `export * as ns from './x'` forwards x under a namespace alias
(export_statement "*" @import.star source: (string) @import.module)
(export_statement (namespace_export (identifier) @import.alias) source: (string) @import)

; anonymous default exports are addressable as `default`; the exported
; value's shape picks the kind (engine prefers the non-variable kind)
(export_statement "default" @name value: (_)) @def.variable
(export_statement "default" @name value: [(function_expression) (arrow_function)]) @def.function
(export_statement "default" @name value: (class)) @def.class

(call_expression function: (identifier) @ref.call)
(call_expression function: (member_expression property: (property_identifier) @ref.call) @refpath)
; `new Foo()` calls Foo (instantiation is a call, as in Java/C#)
(new_expression constructor: (identifier) @ref.call)
(new_expression constructor: (member_expression property: (property_identifier) @ref.call) @refpath)

; local bindings that shadow outer names
(statement_block (lexical_declaration (variable_declarator name: (identifier) @local)))
(required_parameter pattern: (identifier) @local)
(optional_parameter pattern: (identifier) @local)
(arrow_function parameter: (identifier) @local)
(catch_clause parameter: (identifier) @local)
(for_statement initializer: (lexical_declaration (variable_declarator name: (identifier) @local)))
(for_in_statement left: (identifier) @local)
