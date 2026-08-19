// A greeting contract.
export interface Greeter {
  greet(name: string): string;
}

// Console implementation of Greeter.
export class ConsoleGreeter implements Greeter {
  greet(name: string): string {
    return "hi " + name;
  }
}

// Interface inheritance.
export interface LoudGreeter extends Greeter {
  volume(): number;
}
