package shape

// Base carries a name.
type Base struct {
	name string
}

// Name returns the name.
func (b Base) Name() string {
	return b.name
}
