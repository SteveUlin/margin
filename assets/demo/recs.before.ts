import { sampleBeta } from "./beta";

export function recommend(items: Item[]) {
  const d = items.map(sampleBeta);
  return d.sort((a, b) => b - a);
}
