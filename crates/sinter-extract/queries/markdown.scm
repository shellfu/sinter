; Markdown extraction — capture contract in language.rs. Data, not code.
;
; Prose docs are structural facts: a heading declares a section, sections
; nest by level (the block grammar already nests `section` nodes), and the
; first paragraph under a heading is the section's doc — enough for ask's
; doc channel without dumping whole bodies.
;
; tree-sitter markdown splits block and inline grammars: this query runs
; on the BLOCK tree; `[text](target)` links live in the inline grammar,
; declared via LanguageSpec.inline and captured in markdown-inline.scm.
; Stated boundary: setext headings (underlined) are skipped — their name
; node is a whole paragraph.

(section (atx_heading heading_content: (inline) @name)) @def.section

; First paragraph immediately after the heading is the section doc.
(section (atx_heading) . (paragraph (inline) @doc))
