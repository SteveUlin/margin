import { sampleBeta } from "./beta";
import { seeded } from "./rng";

export function recommend(items: Item[], name: string) {
  const rng = seeded(name);
  const d = items.map((i) => sampleBeta(i, rng));
  return d.sort((a, b) => b - a);
}
