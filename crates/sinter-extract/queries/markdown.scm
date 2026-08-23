; Markdown extraction — capture contract in language.rs. Data, not code.
;
; Prose docs are structural facts: a heading declares a section, sections
; nest by level (the block grammar already nests `section` nodes), and the
; blocks under a heading (up to its first subheading) are the section's doc.
;
; tree-sitter markdown splits block and inline grammars: this query runs
; on the BLOCK tree; `[text](target)` links live in the inline grammar,
; declared via LanguageSpec.inline and captured in markdown-inline.scm.
; Stated boundary: setext headings (underlined) are skipped — their name
; node is a whole paragraph.

(section (atx_heading heading_content: (inline) @name)) @def.section

; Every block directly under the heading is the section's doc; nested
; `section` nodes are excluded by listing block kinds, so a section's doc
; stops at its first subheading. Multiple @doc captures on one
; definition are joined with blank lines by the extractor.
(section (atx_heading) [
  (paragraph)
  (list)
  (pipe_table)
  (block_quote)
  (fenced_code_block)
  (indented_code_block)
  (html_block)
] @doc)
