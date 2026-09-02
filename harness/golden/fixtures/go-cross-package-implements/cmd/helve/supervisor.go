package main

import "example.com/helve/internal/supervisor"

// ptyStarter starts through a pty.
type ptyStarter struct{}

// Start launches the command.
func (ptyStarter) Start(cmd string, size supervisor.Size) error {
	return nil
}

// carStarter shares the method name but not its shape.
type carStarter struct{}

// Start turns the key.
func (carStarter) Start() error {
	return nil
}
