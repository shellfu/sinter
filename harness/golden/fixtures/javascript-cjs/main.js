const { double } = require("./lib");
const lib = require("./lib");

// Entry point.
function main() {
  return double(21) + lib.double(2);
}
