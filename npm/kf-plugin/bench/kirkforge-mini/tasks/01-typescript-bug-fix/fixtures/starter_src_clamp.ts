// BUGGY starter: the wrong branch is checked first. A value below min is
// returned (because we only catch the greater-than case). The worker must
// fix this without changing the function signature.
export function clamp(value: number, min: number, max: number): number {
  if (value > min) return max;   // BUG: should be `value > max`
  if (value < max) return min;   // BUG: should be `value < min`
  return value;
}
