package metrics

// Counter counts events.
type Counter struct {
	n int
}

// Value reads the count (value receiver).
func (c Counter) Value() int {
	return c.n
}

// Inc bumps the count (pointer receiver).
func (c *Counter) Inc() {
	c.n++
}

// Reset zeroes the counter.
func (c *Counter) Reset() {
	c.n = 0
}
