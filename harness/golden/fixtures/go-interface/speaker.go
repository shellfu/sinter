package main

// Speaker says things.
type Speaker interface {
	Speak() string
}

// Compile-time assertion: Dog satisfies Speaker.
var _ Speaker = (*Dog)(nil)

// Announce speaks through the interface.
func Announce(s Speaker) string {
	return s.Speak()
}
