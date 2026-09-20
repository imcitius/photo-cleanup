// Russian is the reference dictionary, and it is `as const`, so every value
// has a literal type. A translation has to match its *keys*, not its words —
// this widens the literals back to plain strings so the compiler checks the
// shape and nothing else.
//
// What that buys: a missing key is a build failure, and a key nobody uses
// any more is one too.
export type Shape<T> = {
  [K in keyof T]: T[K] extends readonly string[]
    ? readonly string[]
    : T[K] extends string
      ? string
      : Shape<T[K]>;
};
