; Bash extraction — capture contract in language.rs. Data, not code.

(function_definition name: (word) @name) @def.function

; `source`/`.` bring every function of the sourced file into scope: a
; glob import of the path argument.
(command
  name: (command_name (word) @_cmd)
  argument: [(word) (string) (concatenation) (raw_string)] @import @import.star
  (#any-of? @_cmd "source" "."))

; command invocations are calls (shell functions are called like commands)
(command
  name: (command_name (word) @ref.call)
  (#not-any-of? @ref.call "source" "."))

; local bindings that shadow outer names
(variable_assignment name: (variable_name) @local)
