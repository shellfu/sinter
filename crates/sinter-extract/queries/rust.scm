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

; trait impls: record which trait the block implements (dynamic-dispatch
; pairing) and reference the trait (uses edge + incremental invalidation)
(impl_item
  trait: [
    (type_identifier) @ref.use @trait
    (scoped_type_identifier name: (type_identifier) @ref.use @trait)
    (generic_type type: (type_identifier) @ref.use @trait)
  ]) @trait.impl

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
(let_declaration pattern: (identifier) @local type: (_) @local.type)
(parameter pattern: (identifier) @local type: (_) @local.type)
(let_declaration pattern: (identifier) @local)
(parameter pattern: (identifier) @local)
(closure_parameters (identifier) @local)
(for_expression pattern: (identifier) @local)

; declared field types feed `self.field.method()` resolution
(field_declaration name: (field_identifier) @field.name type: (_) @field.type)

; type references from signatures, bodies, fields, struct literals and
; `Type::assoc` paths: a function that mentions a type depends on it.
; Single-letter names (generics), `Self` and the inferred `_` carry no
; resolvable target.
(scoped_type_identifier name: (type_identifier) @ref.use) @refpath
((type_identifier) @ref.use (#not-match? @ref.use "^(Self|_|[A-Z])$"))
((scoped_identifier path: (identifier) @ref.use) (#match? @ref.use "^[A-Z]") (#not-eq? @ref.use "Self"))

; Embedded SQL at known query sinks, literal strings only (dynamically
; built SQL is never guessed). The engine re-parses each @sql node's
; range with the SQL grammar and merges its captures through the same
; contract .sql files use (see language.rs).

; sqlx compile-checked macros: sqlx::query!("..."), query_as!(T, "..."),
; query_scalar!("..."). The macro requires a literal, so literal-only
; coverage is complete here.
(macro_invocation
  macro: [
    (identifier) @_sql_sink
    (scoped_identifier name: (identifier) @_sql_sink)
  ]
  (token_tree (string_literal (string_content) @sql))
  (#any-of? @_sql_sink "query" "query_as" "query_scalar" "query_unchecked" "query_as_unchecked"))

; Function sinks taking SQL as the first argument: sqlx::query("..."),
; sqlx::query_as::<_, T>("..."), diesel's sql_query("...").
(call_expression
  function: [
    (identifier) @_sql_sink
    (scoped_identifier name: (identifier) @_sql_sink)
    (generic_function function: (identifier) @_sql_sink)
    (generic_function function: (scoped_identifier name: (identifier) @_sql_sink))
  ]
  arguments: (arguments . (string_literal (string_content) @sql))
  (#any-of? @_sql_sink "query" "query_as" "query_scalar" "sql_query"))

; Method sinks (tokio-postgres, rusqlite, connection pools): the SQL is
; the first argument. `.fetch_*` on sqlx builders takes an executor, not
; a string, so it never matches the literal-first-argument shape.
(call_expression
  function: (field_expression field: (field_identifier) @_sql_sink)
  arguments: (arguments . (string_literal (string_content) @sql))
  (#any-of? @_sql_sink
    "query" "query_one" "query_opt" "query_row" "query_map" "query_scalar"
    "execute" "execute_batch" "batch_execute" "prepare" "prepare_cached"))
