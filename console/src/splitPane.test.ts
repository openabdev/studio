import { describe, it, expect } from "vitest";
import { clampWidth, MIN_WIDTH, MAX_WIDTH, DEFAULT_WIDTH } from "./splitPane";

describe("clampWidth", () => {
  it("passes a value already inside the range through unchanged", () => {
    expect(clampWidth(500)).toBe(500);
  });

  it("floors to MIN_WIDTH", () => {
    expect(clampWidth(0)).toBe(MIN_WIDTH);
    expect(clampWidth(-100)).toBe(MIN_WIDTH);
  });

  it("ceils to MAX_WIDTH", () => {
    expect(clampWidth(5000)).toBe(MAX_WIDTH);
  });

  it("rounds fractional pixels", () => {
    expect(clampWidth(500.6)).toBe(501);
  });

  it("falls back to DEFAULT_WIDTH for non-finite input", () => {
    expect(clampWidth(NaN)).toBe(DEFAULT_WIDTH);
    expect(clampWidth(Infinity)).toBe(DEFAULT_WIDTH);
  });
});
