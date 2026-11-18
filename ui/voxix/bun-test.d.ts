/**
 * Ambient typing for the Bun test runner, used only by colocated unit tests of
 * pure modules (e.g. the shortcut-recorder token helpers). The SDK package has
 * its own test setup and does not need this.
 */
declare module "bun:test" {
  export const describe: (name: string, factory: () => void) => void;
  export const test: (name: string, factory: () => void | Promise<void>) => void;
  export const expect: <T>(actual: T) => {
    toBe: (expected: T) => void;
    toEqual: <E>(expected: E) => void;
  };
}
