package app

func init() {
	register("b")
}

func helper(s string) string {
	return s
}

func register(name string) {
	names = append(names, name)
}

var names []string
