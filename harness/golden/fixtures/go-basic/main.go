package main

import (
	"fmt"

	"example.com/go-basic/pkg/util"
)

// Greet returns a greeting.
func Greet(name string) string {
	return "hello " + name
}

type Server struct {
	Port int
}

// Start runs the server.
func (s *Server) Start() error {
	fmt.Println(util.Reverse(Greet("world")))
	return nil
}

type Handler interface {
	Handle() error
}

const MaxRetries = 3

var debug = false
