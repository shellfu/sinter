package app

func init() {
	register("a")
}

// Run drives the pipeline.
func Run() string {
	return helper("run")
}
