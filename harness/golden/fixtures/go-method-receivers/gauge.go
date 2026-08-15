package metrics

// Gauge tracks a level.
type Gauge struct {
	level int
}

// Reset zeroes the gauge.
func (g *Gauge) Reset() {
	g.level = 0
}

// ResetAll zeroes both.
func ResetAll(c *Counter, g *Gauge) {
	c.Reset()
	g.Reset()
}
