import { HTTPException } from "./http-exception";
import * as errors from "./http-exception";

// Rejects oversized bodies.
export function bodyLimit(size: number): void {
  if (size > 1) {
    throw new HTTPException();
  }
  new errors.HTTPException();
}
