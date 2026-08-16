; Proto extraction — capture contract in language.rs. Data, not code.
; message -> struct, enum -> enum, service -> interface, rpc -> method.
; Fields are not symbols in v1; their types are @ref.use sites.

(message (message_name) @name) @def.struct
(enum (enum_name) @name) @def.enum
(service (service_name) @name) @def.interface
(rpc (rpc_name) @name) @def.method

; import "contracts/common.proto"; — quotes stripped by the engine; the
; literal repo path binds the imported file node exactly. Glob semantics
; (like cpp #include / bash source): an imported file's top-level names
; are referencable bare when packages match — protoc rejects bare
; cross-package refs, so glob binding cannot mislabel compiling code.
(import path: (string) @import @import.star)

; Message/enum types in rpc signatures and field types. Scalars (int32,
; string, ...) are anonymous tokens in `type`, never message_or_enum_type,
; so they cannot over-capture. Bare vs dotted split by text predicate:
; last-child anchors are not enforced around the anonymous `.` separators
; in this grammar, so a dotted path captures the whole node as both the
; reference and its path (`contracts.common.Money`).
(rpc ((message_or_enum_type) @ref.use (#not-match? @ref.use "\\.")))
(rpc ((message_or_enum_type) @ref.use @refpath (#match? @ref.use "\\.")))
(field (type ((message_or_enum_type) @ref.use (#not-match? @ref.use "\\."))))
(field (type ((message_or_enum_type) @ref.use @refpath (#match? @ref.use "\\."))))

; oneof branches and map value types are reference sites too (mined from
; sondera-rs: `oneof category { ActionEvent action = 1; }`,
; `map<string, google.protobuf.Value> variables = 5;`).
(oneof_field (type ((message_or_enum_type) @ref.use (#not-match? @ref.use "\\."))))
(oneof_field (type ((message_or_enum_type) @ref.use @refpath (#match? @ref.use "\\."))))
; map key types are their own node (key_type, scalar-only); (type) is the
; value type, so this cannot over-capture keys.
(map_field (type ((message_or_enum_type) @ref.use (#not-match? @ref.use "\\."))))
(map_field (type ((message_or_enum_type) @ref.use @refpath (#match? @ref.use "\\."))))
