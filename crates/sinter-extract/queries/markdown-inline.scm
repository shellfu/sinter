; Markdown INLINE extraction — runs over the block tree's `inline` ranges
; (LanguageSpec.inline; see markdown.scm for the block pack). Same capture
; contract as every pack. Data, not code.
;
; `[text](target)`: the destination is a document path — a use reference
; the resolver binds by exact corpus file path (file_refs), `#fragment`
; landing on the section whose heading slugifies to it. Evidence or
; nothing: dead links stay unresolved, and external URLs (any `scheme:`)
; are filtered right here — no nodes, no edges, ever.
;
; Stated boundary: reference-style links (`[text][ref]`), autolinks, and
; images are out of scope.

((inline_link
   (link_destination) @ref.use @refpath)
 (#not-match? @ref.use "^[a-zA-Z][a-zA-Z0-9+.-]*:"))
