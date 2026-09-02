import { Widget } from "./lib";

// Instantiates a widget.
export function make() {
  return new Widget();
}

// Module surface.
export default { make };
