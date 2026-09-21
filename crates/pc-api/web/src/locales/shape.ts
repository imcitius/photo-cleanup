// Russian is the reference dictionary, and it is `as const`, so every value
// has a literal type. A translation has to match its *keys*, not its words —
// this widens the literals back to plain strings so the compiler checks the
// shape and nothing else.
//
// What that buys: a key missing from a translation is a build failure. It
// says nothing about whether anyone still asks for that key — the dictionaries
// are also read by name at run time, so only a look at the sources can tell.
// `node audit-artifacts/check-unused.cjs` does that look.
export type Shape<T> = {
  [K in keyof T]: T[K] extends readonly string[]
    ? readonly string[]
    : T[K] extends string
      ? string
      : Shape<T[K]>;
};
