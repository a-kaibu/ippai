// Types of suite.mjs for web/src/devbench.test.ts (which checks that the device benchmark's
// program cases are the suite's).
export const VERSION: number;
export const MIN_ROUNDS: Record<string, number>;
export const GROUPS: Record<string, { exe: string; args: string }[]>;
