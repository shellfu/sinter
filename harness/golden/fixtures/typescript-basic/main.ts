import { helper } from "./util";
import { join } from "path";

// Greets a person by name.
export function greet(name: string): string {
  return "hello " + name;
}

export class App {
  run(): void {
    console.log(greet(helper("world")));
  }
}
