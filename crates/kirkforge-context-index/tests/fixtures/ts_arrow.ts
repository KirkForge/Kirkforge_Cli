// TypeScript arrow function exports — exercises WO 8.9 edge case 1.
// `export const foo = () => {}` should be extracted as a Function symbol named "foo".
export const foo = () => {};
export const bar = (x: number) => x * 2;
export const baz: number = 42;
export function declared() {}
