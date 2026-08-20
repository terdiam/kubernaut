import { describe, expect, it } from "vitest";
import { middleTruncate } from "./truncate";

describe("middleTruncate", () => {
  it("leaves short names alone", () => {
    expect(middleTruncate("production/api", 46)).toBe("production/api");
  });

  it("keeps the suffix that distinguishes generated names", () => {
    // Two pods of one CronJob: end-truncation would render them identically.
    const a = "production/cleanup-orphaned-artifacts-29283840-abcde";
    const b = "production/cleanup-orphaned-artifacts-29283840-zyxwv";
    const [shortA, shortB] = [middleTruncate(a, 46), middleTruncate(b, 46)];
    // Guard the premise: a fixture short enough to pass through untouched
    // would make the rest of this test prove nothing.
    expect(shortA).toContain("…");
    expect(shortA).not.toBe(shortB);
    expect(shortA.endsWith("abcde")).toBe(true);
    expect(shortB.endsWith("zyxwv")).toBe(true);
  });

  it("never exceeds the limit", () => {
    expect(middleTruncate("a".repeat(200), 46)).toHaveLength(46);
  });
});
