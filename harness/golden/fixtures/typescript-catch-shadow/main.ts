function handler(): void {}

export function guard(fn: () => void): void {
  try {
    fn();
  } catch (handler) {
    handler();
  }
}
