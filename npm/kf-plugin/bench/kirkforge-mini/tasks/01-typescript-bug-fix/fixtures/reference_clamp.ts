// Reference correct implementation. Used to generate the test cases in
// validator.sh. Not copied into the worker's workspace.
export function clamp(value: number, min: number, max: number): number {
  if (value < min) return min;
  if (value > max) return max;
  return value;
}
