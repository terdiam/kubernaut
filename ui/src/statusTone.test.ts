import { describe, expect, it } from "vitest";
import { toneFor, toneForValue } from "./statusTone";

describe("toneFor", () => {
  it("only colours columns that hold a status", () => {
    expect(toneFor("Status", "Running")).toBe("ok");
    // A pod named "failed-jobs-cleaner" must not turn its Name cell red.
    expect(toneFor("Name", "failed-jobs-cleaner")).toBeNull();
    expect(toneFor("Message", "error while reconciling")).toBeNull();
  });

  it("maps the common vocabularies", () => {
    expect(toneFor("Status", "Ready")).toBe("ok");
    expect(toneFor("Status", "Bound")).toBe("ok");
    expect(toneFor("Sync Status", "Synced")).toBe("ok");
    expect(toneFor("Status", "Pending")).toBe("pending");
    expect(toneFor("Status", "CrashLoopBackOff")).toBe("error");
    expect(toneFor("Status", "Terminating")).toBe("warn");
  });

  /// A cordoned node is `Ready,SchedulingDisabled`; the cordon is the part
  /// worth noticing, so the more severe tone wins.
  it("takes the most severe part of a compound value", () => {
    expect(toneFor("Status", "Ready,SchedulingDisabled")).toBe("warn");
    expect(toneFor("Status", "Ready,NotReady")).toBe("error");
  });

  it("recognises controller-invented reasons by suffix", () => {
    expect(toneForValue("SomethingNewBackOff")).toBe("error");
    expect(toneForValue("CreateContainerConfigError")).toBe("error");
    expect(toneForValue("ThingFailed")).toBe("error");
  });

  it("returns null for words it does not know", () => {
    expect(toneForValue("Chartreuse")).toBeNull();
  });

  it("is case-insensitive", () => {
    expect(toneFor("status", "RUNNING")).toBe("ok");
  });
});
