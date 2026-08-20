import { describe, it, expect } from "vitest";
import { clampHeight, MIN_HEIGHT, MAX_HEIGHT, DEFAULT_HEIGHT } from "./rowResize";

describe("clampHeight", () => {
  it("passes a value already inside the range through unchanged", () => {
    expect(clampHeight(500)).toBe(500);
  });

  it("floors to MIN_HEIGHT", () => {
    expect(clampHeight(0)).toBe(MIN_HEIGHT);
    expect(clampHeight(-100)).toBe(MIN_HEIGHT);
  });

  it("ceils to MAX_HEIGHT", () => {
    expect(clampHeight(5000)).toBe(MAX_HEIGHT);
  });

  it("rounds fractional pixels", () => {
    expect(clampHeight(500.6)).toBe(501);
  });

  it("falls back to DEFAULT_HEIGHT for non-finite input", () => {
    expect(clampHeight(NaN)).toBe(DEFAULT_HEIGHT);
    expect(clampHeight(Infinity)).toBe(DEFAULT_HEIGHT);
  });
});
