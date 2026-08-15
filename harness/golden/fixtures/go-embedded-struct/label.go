package shape

// Circle embeds Base.
type Circle struct {
	Base
	r int
}

// Describe uses the promoted method.
func Describe(c Circle) string {
	return c.Name()
}
