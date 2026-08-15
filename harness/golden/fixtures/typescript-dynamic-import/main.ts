export async function load(): Promise<number> {
  const mod = await import("./heavy");
  return mod.crunch();
}
