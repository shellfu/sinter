; C extraction — capture contract in language.rs. Data, not code.
; .c files only; headers parse under the cpp pack (deliberate split).

; function definitions, including pointer-returning (`int *f()`)
(function_definition
  declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name))) @def.function

; prototypes: `void f(int);` at any level
(declaration
  declarator: (function_declarator declarator: (identifier) @name)) @def.function

; specifiers double as forward declarations (`struct Foo x;`);
; only bodied ones are definitions
(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @def.struct
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @def.struct
(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @def.enum

(type_definition declarator: (type_identifier) @name) @def.typealias

; #define — object-like and function-like
(preproc_def name: (identifier) @name) @def.macro
(preproc_function_def name: (identifier) @name) @def.macro

; includes splice the header textually — glob semantics. Quoted paths are
; repo-relative; <system> includes stay external unless a corpus module
; matches. Headers themselves belong to the cpp pack, but `bar.h` and
; `bar.c` share module path ["bar"], so quoted includes bridge cross-pack.
(preproc_include path: (string_literal) @import @import.star)
(preproc_include path: (system_lib_string) @import @import.star)

(call_expression function: (identifier) @ref.call)

; local bindings that shadow outer names
(declaration declarator: (identifier) @local)
(declaration declarator: (init_declarator declarator: (identifier) @local))
(parameter_declaration declarator: (identifier) @local)
(parameter_declaration
  declarator: (pointer_declarator declarator: (identifier) @local))
