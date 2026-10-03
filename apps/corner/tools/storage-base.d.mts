/**
 * Types for `storage-base.mjs`, which is plain ESM because `tools/hoist-headers.mjs` is a Node
 * script rather than part of the TypeScript project. `vite.config.ts` is type-checked by
 * `tsc -b`, so the import needs a declaration.
 */
export declare function storageBase(): string;
