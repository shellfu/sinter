; Markdown extraction — capture contract in language.rs. Data, not code.
;
; Prose docs are structural facts: a heading declares a section, sections
; nest by level (the block grammar already nests `section` nodes), and the
; first paragraph under a heading is the section's doc — enough for ask's
; doc channel without dumping whole bodies.
;
; Stated boundary: tree-sitter markdown splits block and inline grammars;
; inline content (links, emphasis) is a separate parse the one-grammar
; engine does not run. So `[text](target)` reference edges are out of
; scope for this pack — headings and section docs only. Setext headings
; (underlined) are also skipped: their name node is a whole paragraph.

(section (atx_heading heading_content: (inline) @name)) @def.section

; First paragraph immediately after the heading is the section doc.
(section (atx_heading) . (paragraph (inline) @doc))
