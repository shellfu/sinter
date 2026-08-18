; SQL extraction — capture contract in language.rs. Data, not code.
; Tables and views are the "types" of a schema: table -> struct,
; view -> typealias (a named, stored query), function -> function,
; index -> constant (named auxiliary object; nearest honest kind).
; Column-level modeling is out of scope. CREATE PROCEDURE is NOT
; captured: tree-sitter-sequel 0.3 misparses it (ERROR node).

(create_table (object_reference) @name) @def.struct
(create_view (object_reference) @name) @def.typealias
(create_function (object_reference) @name) @def.function
(create_index (identifier) @name) @def.constant

; Table references. `relation` wraps every FROM / JOIN / UPDATE target
; (its object_reference is the table; a trailing identifier is the
; alias, not captured). INSERT INTO and CREATE INDEX ... ON name their
; table as a direct object_reference child. A column's REFERENCES
; clause (foreign key) is a table use owned by the defining table.
(relation (object_reference) @ref.use)
(insert (object_reference) @ref.use)
(create_index (object_reference) @ref.use)
(column_definition (object_reference) @ref.use)
