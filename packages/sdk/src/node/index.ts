/** Node subpath: file helpers + the embedded-child `create()`. This module
 * is imported only from Node/Bun-shaped runtimes (spec §10 node helpers). */
export {
  create,
  parseReadyLine,
  type CreateOptions,
  type CreateOptions as CreateDerivedOptions,
  type ChildProcessLike,
  type EmbeddedClient,
  type ReadyLine,
  type ReadableLike,
  type SpawnLike,
} from "./create.js";
export { readWavHeader, wavToMonoPcm16, type WavInfo } from "./file.js";
