import { describe, expect, it } from "vitest";
import { formatClock, formatDateTime, localiseLogLine } from "./time";

describe("timestamp formatting", () => {
  /// The reason the setting exists: the same instant reads seven hours apart.
  it("renders one instant differently per zone", () => {
    const iso = "2026-08-19T14:30:23Z";
    const jakarta = formatDateTime(iso, "Asia/Jakarta");
    const utc = formatDateTime(iso, "UTC");
    expect(jakarta).not.toEqual(utc);
    expect(jakarta).toContain("21:30:23");
    expect(utc).toContain("14:30:23");
  });

  /// 24-hour, so a timestamp here lines up with one in a log without mental
  /// arithmetic.
  it("uses a 24-hour clock regardless of locale", () => {
    const formatted = formatDateTime("2026-08-19T14:30:23Z", "UTC");
    expect(formatted).not.toMatch(/AM|PM/);
  });

  it("passes through values it cannot parse rather than showing NaN", () => {
    expect(formatDateTime("not a date", "UTC")).toBe("not a date");
    expect(formatDateTime(null, "UTC")).toBe("—");
  });

  it("formats chart clock times in the chosen zone", () => {
    const at = Date.parse("2026-08-19T14:30:00Z");
    expect(formatClock(at, "UTC")).toBe("14:30");
    expect(formatClock(at, "Asia/Jakarta")).toBe("21:30");
  });

  /// Kubernetes prefixes log lines with UTC; reading those beside a local
  /// clock is the confusion being removed.
  it("converts the timestamp Kubernetes prefixes to a log line", () => {
    const line = "2026-08-19T14:30:23.123456789Z something happened";
    const converted = localiseLogLine(line, "Asia/Jakarta");
    expect(converted).toContain("21:30:23");
    expect(converted).toContain("something happened");
    expect(converted).not.toContain("2026-08-19T");
  });

  /// A line without a timestamp must survive untouched — including one that
  /// merely mentions a date.
  it("leaves lines without a leading timestamp alone", () => {
    const plain = "GET /healthz 200";
    expect(localiseLogLine(plain, "UTC")).toBe(plain);

    const mentions = "backup for 2026-08-19T00:00:00Z finished";
    expect(localiseLogLine(mentions, "Asia/Jakarta")).toBe(mentions);
  });
});
