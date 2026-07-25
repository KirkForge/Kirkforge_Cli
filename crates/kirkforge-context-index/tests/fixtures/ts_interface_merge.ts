// TypeScript interface merging — exercises WO 8.9 edge case 2.
// Two `interface Foo` declarations in the same file. After dedup, the index
// should have exactly one `Foo` entry per file (or, in the cross-file merge
// case, exactly one per file per the test that runs both files together).
interface Foo {
  a: number;
}

interface Foo {
  b: string;
}

interface Bar {
  c: boolean;
}
