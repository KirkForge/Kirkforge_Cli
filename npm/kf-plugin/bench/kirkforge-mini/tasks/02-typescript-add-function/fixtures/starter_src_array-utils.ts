// Starter file. The worker must ADD a `unique` function without modifying
// or removing the existing exports.
export function first<T>(arr: T[]): T | undefined {
  return arr[0];
}

export function last<T>(arr: T[]): T | undefined {
  return arr[arr.length - 1];
}
