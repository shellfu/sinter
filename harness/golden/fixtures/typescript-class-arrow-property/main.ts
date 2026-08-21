export class Handler {
  fetch = (request: string): string => {
    return normalize(request);
  };

  handle = function (request: string): string {
    return normalize(request);
  };
}

function normalize(value: string): string {
  return value;
}
