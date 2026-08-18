; JavaScript extraction — capture contract in language.rs. Data, not code.
; Roughly typescript.scm minus types, plus CommonJS require() and JSX.

(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method

; top-level (possibly exported) bindings only; function-body consts are
; locals, not symbols. Arrow/function values are functions (engine prefers
; the non-variable kind when two patterns claim one node).
(program (lexical_declaration (variable_declarator name: (identifier) @name)) @def.variable)
(program (variable_declaration (variable_declarator name: (identifier) @name)) @def.variable)
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

; CommonJS: `const lib = require("./lib")` aliases the module;
; `const { f } = require("./lib")` binds the item like a named import.
(variable_declarator
  name: (identifier) @import.alias
  value: (call_expression
    function: (identifier) @_require
    arguments: (arguments (string) @import))
  (#eq? @_require "require"))
(variable_declarator
  name: (object_pattern (shorthand_property_identifier_pattern) @import.name)
  value: (call_expression
    function: (identifier) @_require
    arguments: (arguments (string) @import.module))
  (#eq? @_require "require"))

(call_expression function: (identifier) @ref.call)
(call_expression function: (member_expression property: (property_identifier) @ref.call) @refpath)

; JSX components: <Foo/> uses Foo; lowercase tags are host elements, not refs
(jsx_opening_element name: (identifier) @ref.use (#match? @ref.use "^[A-Z]"))
(jsx_self_closing_element name: (identifier) @ref.use (#match? @ref.use "^[A-Z]"))

; local bindings that shadow outer names
(statement_block (lexical_declaration (variable_declarator name: (identifier) @local)))
(statement_block (variable_declaration (variable_declarator name: (identifier) @local)))
(formal_parameters (identifier) @local)
(arrow_function parameter: (identifier) @local)
(catch_clause parameter: (identifier) @local)
(for_statement initializer: (lexical_declaration (variable_declarator name: (identifier) @local)))
(for_in_statement left: (identifier) @local)
