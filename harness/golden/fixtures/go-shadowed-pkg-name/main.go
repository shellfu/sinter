package demo

import "example.com/fx/pkg/util"

// Buffer holds data.
type Buffer struct {
	data string
}

// Reverse reverses the buffer contents.
func (b Buffer) Reverse(s string) string {
	return s + b.data
}

// Lib reverses via the util package.
func Lib(s string) string {
	return util.Reverse(s)
}

// Local reverses via a shadowing local variable.
func Local(s string) string {
	util := Buffer{data: s}
	return util.Reverse(s)
}
