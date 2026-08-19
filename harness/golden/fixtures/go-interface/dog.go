package main

// Dog barks.
type Dog struct{}

// Speak returns a bark.
func (d *Dog) Speak() string {
	return "woof"
}
