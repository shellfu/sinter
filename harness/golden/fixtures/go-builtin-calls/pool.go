package pool

// New builds a fresh pool.
func New(size int) []string {
	items := make([]string, 0, size)
	items = append(items, "seed")
	_ = len(items)
	return items
}

// Grow doubles the pool.
func Grow(items []string) []string {
	out := new([]string)
	*out = append(*out, items...)
	if len(items) == 0 {
		return New(cap(items))
	}
	return *out
}
