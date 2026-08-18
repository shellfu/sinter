; C# extraction — capture contract in language.rs. Data, not code.
;
; Identity model: path-derived, namespace-aligned. A file's module is its
; directory (csharp_module_path), and `using Acme.Util;` is a glob import
; of that directory — exactly Go's package split. Namespace declarations
; are NOT captured: the contract has no channel for declaration-derived
; module identity without engine changes, so resolution assumes
; directories mirror namespaces (the dominant .NET layout convention).

(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(struct_declaration name: (identifier) @name) @def.struct
(record_declaration name: (identifier) @name) @def.class
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
; properties are members of their type
(property_declaration name: (identifier) @name) @def.field

; `using Ns;` imports a namespace, not a type: every type in it binds (glob).
(using_directive !name [(identifier) (qualified_name)] @import.module @import.star)
; `using F = Acme.Util.Foo;` binds the alias to the full path.
(using_directive name: (identifier) @import.alias [(identifier) (qualified_name)] @import)

(invocation_expression function: (identifier) @ref.call)
(invocation_expression
  function: (member_access_expression name: (identifier) @ref.call) @refpath)

; object creation is a call on the type (constructor)
(object_creation_expression type: (identifier) @ref.call)
(object_creation_expression
  type: (qualified_name name: (identifier) @ref.call) @refpath)

; local bindings that shadow outer names; typed forms carry @local.type
; (untyped pattern first: the typed entry must win local_at's next_back)
(local_declaration_statement
  (variable_declaration (variable_declarator name: (identifier) @local)))
(local_declaration_statement
  (variable_declaration
    type: (identifier) @local.type
    (variable_declarator name: (identifier) @local))
  (#not-eq? @local.type "var"))
(parameter name: (identifier) @local)
(parameter type: (identifier) @local.type name: (identifier) @local)
