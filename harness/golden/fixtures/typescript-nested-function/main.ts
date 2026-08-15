export function outer(): number {
  function inner(): number {
    return leaf();
  }
  return inner();
}

function leaf(): number {
  return 1;
}
