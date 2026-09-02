package supervisor

// Size is a terminal size.
type Size struct{}

// Starter launches a process.
type Starter interface {
	Start(cmd string, size Size) error
}

// Serve starts through the interface.
func Serve(s Starter) error {
	return s.Start("sh", Size{})
}
