import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ObjectContext } from "./ObjectContext";
import { PodUsageValue } from "./UsageBar";
import type { PodUsage, Related } from "../types";

const api = vi.hoisted(() => ({
  relatedResources: vi.fn(),
  objectEvents: vi.fn(),
  podEvents: vi.fn(),
  podUsages: vi.fn(),
  nodeSummaries: vi.fn(),
}));

vi.mock("../api", () => ({ api, startWatch: vi.fn() }));

const usage = (name: string, over: Partial<PodUsage> = {}): PodUsage => ({
  namespace: "shop",
  name,
  cpuUsage: 0.25,
  cpuRequests: 0.1,
  cpuLimits: 0.5,
  memoryUsage: 128 * 1024 * 1024,
  memoryRequests: 64 * 1024 * 1024,
  memoryLimits: 0,
  usageAvailable: true,
  ...over,
});

const podRef = (name: string) => ({
  kind: "Pod",
  name,
  namespace: "shop",
  resource: "core/v1/pods",
  detail: "Running",
  health: "ok",
});

const related = (pods: string[]): Related => ({
  pods: pods.map(podRef),
  services: [],
  ingresses: [],
  controllers: [],
  config: [],
  storage: [],
  policies: [],
  nodes: [],
});

afterEach(cleanup);

describe("PodUsageValue", () => {
  it("shows a dash rather than zero before metrics-server reports", () => {
    render(<PodUsageValue used={0} request={0} limit={0} format="cores" available={false} />);
    expect(screen.getByText("—")).toBeTruthy();
  });

  it("draws no bar for a pod without a limit — there is no edge to be close to", () => {
    const { container } = render(
      <PodUsageValue used={0.25} request={0.1} limit={0} format="cores" available />,
    );
    expect(screen.getByText("0.25")).toBeTruthy();
    expect(container.querySelector(".usage__track")).toBeNull();
    expect(container.querySelector(".usage")?.getAttribute("title")).toContain("no limit");
  });

  it("colours the bar by how close usage is to the limit", () => {
    const { container } = render(
      <PodUsageValue used={0.48} request={0.1} limit={0.5} format="cores" available />,
    );
    expect(container.querySelector(".usage__fill--critical")).not.toBeNull();
  });
});

describe("ObjectContext pod usage", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.objectEvents.mockResolvedValue([]);
    api.podEvents.mockResolvedValue([]);
    api.nodeSummaries.mockResolvedValue([]);
  });

  it("gives each pod of a workload its own CPU and memory", async () => {
    api.relatedResources.mockResolvedValue(related(["web-a", "web-b"]));
    api.podUsages.mockResolvedValue([
      usage("web-a", { cpuUsage: 0.05, cpuLimits: 0 }),
      usage("web-b", { cpuUsage: 0.4 }),
    ]);

    render(
      <ObjectContext
        cluster="dev"
        resource="apps/v1/deployments"
        kind="Deployment"
        namespace="shop"
        name="web"
        object={null}
        revision={0}
      />,
    );

    await waitFor(() => expect(api.podUsages).toHaveBeenCalledWith("dev", "shop"));
    // Two pods, so two CPU figures — one per replica, not one aggregate.
    await waitFor(() => expect(screen.getByText("0.05")).toBeTruthy());
    expect(screen.getByText("0.4")).toBeTruthy();
    expect(screen.getAllByText("128MiB")).toHaveLength(2);
  });

  it("shows a pod's own usage against its request and limit", async () => {
    api.relatedResources.mockResolvedValue(related(["web-a"]));
    api.podUsages.mockResolvedValue([usage("web-a")]);

    render(
      <ObjectContext
        cluster="dev"
        resource="core/v1/pods"
        kind="Pod"
        namespace="shop"
        name="web-a"
        object={null}
        revision={0}
      />,
    );

    await waitFor(() =>
      expect(screen.getByText(/0\.25 — requests 0\.1, limit 0\.5 \(50%\)/)).toBeTruthy(),
    );
    expect(screen.getByText(/128MiB — requests 64\.0MiB, no limit/)).toBeTruthy();
  });

  it("does not ask for pod usage on a kind that owns no pods", async () => {
    api.relatedResources.mockResolvedValue(related([]));

    render(
      <ObjectContext
        cluster="dev"
        resource="core/v1/configmaps"
        kind="ConfigMap"
        namespace="shop"
        name="settings"
        object={null}
        revision={0}
      />,
    );

    await waitFor(() => expect(api.relatedResources).toHaveBeenCalled());
    expect(api.podUsages).not.toHaveBeenCalled();
  });
});
