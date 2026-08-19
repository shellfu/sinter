; C++ extraction — capture contract in language.rs. Data, not code.

; free functions
(function_definition
  declarator: (function_declarator declarator: (identifier) @name)) @def.function

; methods defined in-body inside a class/struct
(function_definition
  declarator: (function_declarator declarator: (field_identifier) @name)) @def.method

; out-of-class definitions: `void Foo::bar() {}` qualifies under Foo
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (namespace_identifier) @qualifier
      name: (identifier) @name))) @def.method

; specifiers double as forward declarations (`struct Foo x;`);
; only bodied ones are definitions
(class_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @def.class
(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @def.struct

; method declarations inside class bodies; the enclosing class scopes them
(field_declaration
  declarator: (function_declarator declarator: (field_identifier) @name)) @def.method

; base classes: reference + inherited-member promotion (@embed) +
; dispatch pairing (@trait/@trait.impl) — virtual-override fan-out
; approximation, mirroring the C# base_list treatment
(class_specifier
  (base_class_clause
    (type_identifier) @ref.use @embed @trait)
  body: (field_declaration_list)) @trait.impl

; namespaces scope their contents
(namespace_definition name: (namespace_identifier) @name) @def.module

(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @def.enum

; includes splice the header textually: every top-level name becomes
; visible — glob semantics, like bash `source`. Quoted paths have their
; quotes stripped by the engine; <system> brackets by cpp_absolutize.
; System includes stay external unless a corpus module matches.
(preproc_include path: (string_literal) @import @import.star)
(preproc_include path: (system_lib_string) @import @import.star)

(call_expression function: (identifier) @ref.call)
(call_expression
  function: (qualified_identifier name: (identifier) @ref.call) @refpath)
(call_expression
  function: (field_expression field: (field_identifier) @ref.call) @refpath)

; local bindings that shadow outer names; typed forms carry @local.type
; for method binding through a typed local
(declaration declarator: (identifier) @local)
(declaration
  type: (type_identifier) @local.type
  declarator: (identifier) @local)
(declaration declarator: (init_declarator declarator: (identifier) @local))
(declaration
  type: (type_identifier) @local.type
  declarator: (init_declarator declarator: (identifier) @local))
(parameter_declaration declarator: (identifier) @local)
(parameter_declaration
  type: (type_identifier) @local.type
  declarator: (identifier) @local)

; Unreal-style classes: an export macro between `class` and the name makes
; tree-sitter take the macro as the class name and leave the real name as
; a stray declarator. Match that shape, gated on the *_API convention.
(declaration
  type: (class_specifier name: (type_identifier) @_api)
  declarator: (identifier) @name
  (#match? @_api "_API$")) @def.class
(function_definition
  type: (class_specifier name: (type_identifier) @_api)
  declarator: (identifier) @name
  (#match? @_api "_API$")) @def.class

; member prototypes inside the misparsed body (plain and `public:`-labeled):
; containment under the recovered class yields Class::member
(function_definition
  type: (class_specifier name: (type_identifier) @_api)
  body: (compound_statement
    (declaration
      declarator: (function_declarator declarator: (identifier) @name)) @def.method)
  (#match? @_api "_API$"))
(function_definition
  type: (class_specifier name: (type_identifier) @_api)
  body: (compound_statement
    (labeled_statement
      (declaration
        declarator: (function_declarator declarator: (identifier) @name)) @def.method))
  (#match? @_api "_API$"))
