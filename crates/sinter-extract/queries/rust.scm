; Rust extraction — capture contract in language.rs. Data, not code.

(function_item name: (identifier) @name) @def.function
(function_signature_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(mod_item name: (identifier) @name) @def.module
(const_item name: (identifier) @name) @def.constant
(static_item name: (identifier) @name) @def.static
(type_item name: (type_identifier) @name) @def.typealias
(macro_definition name: (identifier) @name) @def.macro

; impl blocks scope their items under the type name but are not symbols
(impl_item type: (type_identifier) @name) @scope
(impl_item type: (generic_type type: (type_identifier) @name)) @scope

; imports: plain paths, aliases, lists, globs (pub use re-exports included)
(use_declaration argument: [(identifier) (scoped_identifier)] @import)
(use_declaration
  argument: (use_as_clause path: (_) @import alias: (identifier) @import.alias))
(use_declaration
  argument: (scoped_use_list
    path: (_) @import.module
    list: (use_list [(identifier) (scoped_identifier)] @import.name)))
(use_declaration
  argument: (use_wildcard [(identifier) (scoped_identifier)] @import.module) @import.star)

(call_expression function: (identifier) @ref.call)
(call_expression function: (scoped_identifier name: (identifier) @ref.call) @refpath)
(call_expression function: (field_expression field: (field_identifier) @ref.call) @refpath)
(macro_invocation macro: (identifier) @ref.call)

; local bindings that shadow outer names
(let_declaration pattern: (identifier) @local)
(parameter pattern: (identifier) @local)
(closure_parameters (identifier) @local)
(for_expression pattern: (identifier) @local)
