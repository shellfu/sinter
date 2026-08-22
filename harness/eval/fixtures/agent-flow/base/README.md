# Agent flow fixture

`entry` is the public entry point. It dispatches through `dispatch` to `leaf`.
The fixture deliberately includes an ambiguous `duplicate` name and an
unresolved external call so agent abstention can be measured.
