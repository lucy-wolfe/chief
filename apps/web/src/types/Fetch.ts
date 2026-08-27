/**
 * The injected-fetch dependency, stated as the shape the callers actually
 * use instead of as `typeof fetch`.
 *
 * `typeof fetch` does not name a stable contract — it names whatever the
 * ambient global happens to be in the current type environment. In this
 * workspace `@types/bun` is a root devDependency, so every tsconfig without
 * an explicit `types` list (this app's included) resolves `fetch` to Bun's,
 * which carries `preconnect`. Nothing in this app ever calls `preconnect`;
 * every consumer of an injected fetch does exactly one thing with it, which
 * is call it and await a `Response`. Declaring the dependency as `typeof
 * fetch` therefore demanded a Bun-only method from every test double and
 * from any browser-only implementation — 20 of `apps/web/test`'s type errors
 * were that one over-specification, reported once per stub.
 *
 * A structural type also keeps the services runtime-agnostic on purpose:
 * this is a Next app that runs in a browser and in Node, and neither of those
 * `fetch` globals is Bun's.
 */
export type FetchImpl = (input: string | URL | Request, init?: RequestInit) => Promise<Response>
