import { describe, it, expect } from "vitest";
import { cycleTheme, type Theme } from "./theme";

describe("cycleTheme", () => {
  it("cycles System → Light → Dark → System", () => {
    expect(cycleTheme("system")).toBe("light");
    expect(cycleTheme("light")).toBe("dark");
    expect(cycleTheme("dark")).toBe("system");
  });

  it("returns to a full loop", () => {
    let t: Theme = "system";
    for (let i = 0; i < 3; i++) t = cycleTheme(t);
    expect(t).toBe("system");
  });
});
