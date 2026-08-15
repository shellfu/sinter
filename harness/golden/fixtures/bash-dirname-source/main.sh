#!/usr/bin/env bash

source "$(dirname "$0")/lib/util.sh"

# Entry point.
main() {
  reverse_words "a b c"
}

main
