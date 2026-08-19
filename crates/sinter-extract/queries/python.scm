; Python extraction — capture contract in language.rs. Data, not code.

(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class

; imports: plain, aliased, from (plain/aliased/relative), star
(import_statement name: (dotted_name) @import)
(import_statement
  name: (aliased_import name: (dotted_name) @import alias: (identifier) @import.alias))
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.module
  name: (dotted_name) @import.name)
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.module
  name: (aliased_import name: (dotted_name) @import.name alias: (identifier) @import.alias))
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.module
  (wildcard_import) @import.star)

; base classes: reference + inherited-member promotion (@embed, Go-style)
; + dispatch pairing (@trait/@trait.impl) so overridden methods fan out
; with dynamic evidence. Plain-identifier bases only; keyword arguments
; (metaclass=...) are their own nodes and never match.
(class_definition
  superclasses: (argument_list
    (identifier) @ref.use @embed @trait)) @trait.impl

; docstrings are the doc, overriding preceding # comments
(function_definition
  body: (block . (expression_statement (string (string_content) @doc))))
(class_definition
  body: (block . (expression_statement (string (string_content) @doc))))

(call function: (identifier) @ref.call)
(call function: (attribute attribute: (identifier) @ref.call) @refpath)

; decorator application is a call at definition scope
(decorator (identifier) @ref.call)
(decorator (attribute attribute: (identifier) @ref.call) @refpath)
(decorator (call function: (identifier) @ref.call))
(decorator (call function: (attribute attribute: (identifier) @ref.call) @refpath))

; local bindings that shadow outer names
(parameters (identifier) @local)
(typed_parameter (identifier) @local)
(default_parameter name: (identifier) @local)
(typed_default_parameter name: (identifier) @local)
(assignment left: (identifier) @local)
(for_statement left: (identifier) @local)
(as_pattern alias: (as_pattern_target (identifier) @local))
