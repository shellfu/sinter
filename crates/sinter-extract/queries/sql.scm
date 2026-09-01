; SQL extraction — capture contract in language.rs. Data, not code.
; SQL objects use SQL-specific graph kinds. CREATE PROCEDURE is NOT
; captured: tree-sitter-sequel 0.3 misparses it (ERROR node).

(create_table (object_reference) @name @ref.create) @def.table
(create_view (object_reference) @name @ref.create) @def.view
(create_materialized_view (object_reference) @name @ref.create) @def.view
(create_function (object_reference) @name) @def.function
(create_index (identifier) @name @ref.create) @def.index

; Column definitions are nested definitions owned by the surrounding table.
(create_table
  (column_definitions
    (column_definition
      name: [(identifier) (literal)] @name) @def.column))

; A relation wraps every FROM / JOIN / UPDATE target. UPDATE therefore
; overlaps the generic read capture; the extractor gives writes precedence
; at an identical source span.
(relation (object_reference) @ref.read)
(update (relation (object_reference) @ref.write))
(insert (object_reference) @ref.write)
; DELETE FROM is aliased to `from` and carries the target directly rather
; than through a relation node.
(from (object_reference) @ref.write)

; Schema dependencies are uses rather than data access: an index is defined
; on a table, and a foreign-key column references another table.
(create_index (object_reference) @ref.use)
(column_definition (object_reference) @ref.use)

; Migration lineage. These references are owned by the file, so reverse
; traversal from a table/view/index reaches every migration that changes it.
(alter_table (object_reference) @ref.alter)
(drop_table (object_reference) @ref.drop)
(drop_view (object_reference) @ref.drop)
(drop_index name: (identifier) @ref.drop)
