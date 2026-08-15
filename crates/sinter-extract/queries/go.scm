; Go extraction — capture contract in language.rs. Data, not code.

(function_declaration name: (identifier) @name) @def.function

; methods qualify under their receiver type
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [(type_identifier) @qualifier
             (pointer_type (type_identifier) @qualifier)]))
  name: (field_identifier) @name) @def.method

(type_declaration
  (type_spec name: (type_identifier) @name type: (struct_type)) @def.struct)
(type_declaration
  (type_spec name: (type_identifier) @name type: (interface_type)) @def.interface)
(type_declaration
  (type_spec name: (type_identifier) @name
    type: [(type_identifier) (qualified_type) (map_type) (slice_type)
           (array_type) (function_type) (channel_type) (pointer_type)]) @def.typealias)

; package-level constants and variables only; function bodies hold locals
(source_file (const_declaration (const_spec name: (identifier) @name) @def.constant))
(source_file (var_declaration (var_spec name: (identifier) @name) @def.variable))

; imports: plain, aliased, dot
(import_spec !name path: (interpreted_string_literal) @import)
(import_spec
  name: (package_identifier) @import.alias
  path: (interpreted_string_literal) @import)
(import_spec name: (dot) @import.alias path: (interpreted_string_literal) @import)

(call_expression function: (identifier) @ref.call)
(call_expression function: (selector_expression field: (field_identifier) @ref.call) @refpath)

; type conversions are uses of the type, not calls
(type_conversion_expression
  type: (qualified_type name: (type_identifier) @ref.use) @refpath)
(type_conversion_expression type: (type_identifier) @ref.use)

; local bindings that shadow outer names
; ponytail: `var` inside function bodies not tracked — `:=` covers the
; idiomatic shadow vector; add a body-var pattern if a fixture demands it.
; Typed forms carry @local.type: local type evidence for method binding.
(short_var_declaration left: (expression_list (identifier) @local))
(short_var_declaration
  left: (expression_list (identifier) @local)
  right: (expression_list (composite_literal type: (type_identifier) @local.type)))
(parameter_declaration name: (identifier) @local)
(parameter_declaration
  name: (identifier) @local
  type: [(type_identifier) @local.type
         (pointer_type (type_identifier) @local.type)])
(range_clause left: (expression_list (identifier) @local))

; embedded struct fields promote the embedded type's members
(type_declaration
  (type_spec
    type: (struct_type
      (field_declaration_list
        (field_declaration !name type: (type_identifier) @embed)))))
