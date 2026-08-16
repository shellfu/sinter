; Proto extraction — capture contract in language.rs. Data, not code.
; message -> struct, enum -> enum, service -> interface, rpc -> method.
; Fields are not symbols in v1; their types are @ref.use sites.

(message (message_name) @name) @def.struct
(enum (enum_name) @name) @def.enum
(service (service_name) @name) @def.interface
(rpc (rpc_name) @name) @def.method

; import "contracts/common.proto"; — quotes stripped by the engine; the
; literal repo path binds the imported file node exactly.
(import path: (string) @import)

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
