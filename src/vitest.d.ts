import type { TestingLibraryMatchers } from "@testing-library/jest-dom/matchers";
import "vitest";

// jest-dom 7 augments Vitest's old Assertion interface. Vitest 5 reads
// custom matchers from Matchers and carries their return type separately.
declare module "vitest" {
  interface Matchers<R, T> extends TestingLibraryMatchers<unknown, R> {}
}
