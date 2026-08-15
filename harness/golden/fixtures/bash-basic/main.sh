#!/usr/bin/env bash

# Greets someone.
greet() {
  echo "hello $1"
}

# Runs the pipeline.
run() {
  greet "world"
  grep -q foo /dev/null
}

run
