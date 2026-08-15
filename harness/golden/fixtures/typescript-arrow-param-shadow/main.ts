import { fmt } from "./fmt";

export function render(parts: string[]): string[] {
  return parts.map(fmt => fmt(""));
}
