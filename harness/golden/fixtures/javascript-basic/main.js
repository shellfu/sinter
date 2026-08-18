import { formatLabel } from "./util";
import { useState } from "react";

/** Greets a person by name. */
export function greet(name) {
  return "hello " + name;
}

export class App {
  /** Runs the app. */
  run() {
    useState(0);
    console.log(greet(formatLabel("world")));
  }
}
