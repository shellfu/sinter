function step(): void {}

export function runAll(steps: Array<() => void>): void {
  for (const step of steps) {
    step();
  }
}
