package app

// Config is the exported configuration.
type Config struct {
	Path string
}

// config is the unexported default.
var config = "default"

// Load builds a Config.
func Load() Config {
	return Config{Path: config}
}

// load is the unexported loader.
func load() string {
	return config
}

// Boot wires both.
func Boot() string {
	Load()
	return load()
}
