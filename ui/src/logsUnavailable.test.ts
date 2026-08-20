import { describe, expect, it } from "vitest";
import { duration, explainLogFailure, survivingFacts } from "./logsUnavailable";
import type { ContainerInfo } from "./types";

const container = (over: Partial<ContainerInfo> = {}): ContainerInfo => ({
  name: "cleanup",
  role: "app",
  image: "cleanup:1",
  ready: false,
  restarts: 0,
  state: "terminated",
  reason: "Error",
  exitCode: 1,
  startedAt: "2026-06-07T04:00:20Z",
  finishedAt: "2026-06-07T04:01:08Z",
  ...over,
});

describe("explainLogFailure", () => {
  it("treats a garbage-collected log file as permanent, not as a retry", () => {
    // Verbatim from a 74-day-old CronJob pod on a live cluster.
    const failure = explainLogFailure(
      "unable to retrieve container logs for containerd://143ed0acfa08d6f9e6986f67eca65ace76555f4cd3c5c5e54a06b89ea500ba40",
    );
    expect(failure?.code).toBe("LogsGarbageCollected");
    expect(failure?.transient).toBe(false);
    // The misleading part is that --previous looks like a way out. It is not.
    expect(failure?.remedy).toContain("--previous");
  });

  it("separates an unreachable node from a missing log", () => {
    const failure = explainLogFailure(
      'Get "https://10.0.0.4:10250/containerLogs/production/api-1/api": dial tcp 10.0.0.4:10250: connect: no route to host',
    );
    expect(failure?.code).toBe("NodeUnreachable");
    // The node coming back fixes it, so this one is worth retrying.
    expect(failure?.transient).toBe(true);
  });

  it("recognises the pods/log grant being absent", () => {
    const failure = explainLogFailure(
      'pods "api-1" is forbidden: User "dev" cannot get resource "pods/log" in API group "" in the namespace "production"',
    );
    expect(failure?.code).toBe("Forbidden");
  });

  it("explains an empty previous-instance request", () => {
    const failure = explainLogFailure(
      'previous terminated container "api" in pod "api-1" not found',
    );
    expect(failure?.code).toBe("NoPreviousInstance");
  });

  it("leaves anything it does not recognise alone", () => {
    expect(explainLogFailure("unexpected EOF while reading the stream")).toBeNull();
  });
});

describe("survivingFacts", () => {
  it("reports the exit status and how long the container ran", () => {
    // How long it ran narrows the cause more than the exit code alone: 48
    // seconds rules out a failure to start and rules out a timeout.
    expect(survivingFacts(container())).toEqual([
      "Exited 1 (Error)",
      "Ran for 48s",
      "Started 2026-06-07T04:00:20Z",
      "Finished 2026-06-07T04:01:08Z",
    ]);
  });

  it("has nothing to report for a container with no status", () => {
    expect(survivingFacts(undefined)).toEqual([]);
  });

  it("reports a waiting container by its reason", () => {
    const facts = survivingFacts(
      container({
        state: "waiting",
        reason: "ImagePullBackOff",
        exitCode: null,
        startedAt: null,
        finishedAt: null,
      }),
    );
    expect(facts).toEqual(["waiting: ImagePullBackOff"]);
  });
});

describe("duration", () => {
  it("scales its unit with the length", () => {
    expect(duration("2026-06-07T04:00:20Z", "2026-06-07T04:01:08Z")).toBe("48s");
    expect(duration("2026-06-07T04:00:00Z", "2026-06-07T04:05:30Z")).toBe("5m 30s");
    expect(duration("2026-06-07T04:00:00Z", "2026-06-07T06:30:00Z")).toBe("2h 30m");
  });

  it("refuses the zero start a container that never ran carries", () => {
    // Seen on a live cluster for a StartError container.
    expect(duration("1970-01-01T00:00:00Z", "2026-08-20T13:00:00Z")).toBeNull();
  });

  it("refuses nonsense rather than printing it", () => {
    expect(duration(null, "2026-06-07T04:00:00Z")).toBeNull();
    expect(duration("2026-06-07T04:05:00Z", "2026-06-07T04:00:00Z")).toBeNull();
    expect(duration("not a date", "2026-06-07T04:00:00Z")).toBeNull();
  });
});
