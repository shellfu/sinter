package demo

import "example.com/fx/pkg/model"

// Wrap converts a raw string into a model ID.
func Wrap(raw string) model.ID {
	return model.ID(raw)
}
