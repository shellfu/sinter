package demo

import u "example.com/fx/pkg/text"

// Shout uppercases and punctuates s.
func Shout(s string) string {
	return u.Upper(s) + "!"
}
