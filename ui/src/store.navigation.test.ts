import { beforeEach, describe, expect, it, vi } from "vitest";
import { useStore } from "./store";
import type { DiscoveryCache, ResourceDescriptor, Row, WatchBatch, WatchHandle } from "./types";

const startWatch = vi.fn();

vi.mock("./api", () => ({
  api: { stopWatch: vi.fn(async () => {}) },
  startWatch: (...args: unknown[]) => startWatch(...args),
}));

const pods: ResourceDescriptor = {
  key: "core/v1/pods",
  group: "",
  version: "v1",
  kind: "Pod",
  plural: "pods",
  apiVersion: "v1",
  namespaced: true,
  verbs: ["list", "watch"],
  shortNames: ["po"],
  isCrd: false,
  printerColumns: [],
  watchable: true,
  editable: true,
  deletable: true,
};

const discovery: DiscoveryCache = {
  cluster: "default",
  groups: [{ name: "core", preferredVersion: "v1", resources: [pods] }],
  fetchedAt: "2026-08-20T00:00:00Z",
  crdMetadataAvailable: true,
};

const row: Row = {
  uid: "uid-1",
  name: "cleanup-orphaned-artifacts-29283840-abcde",
  namespace: "production",
  created: null,
  resourceVersion: "1",
  cells: [],
  health: "error",
  terminating: false,
};

const batch = (over: Partial<WatchBatch> = {}): WatchBatch => ({
  epoch: 1,
  snapshot: false,
  upserts: [],
  deletes: [],
  state: { state: "initializing" },
  ...over,
});

describe("openObject", () => {
  beforeEach(() => {
    startWatch.mockReset();
    useStore.setState({
      activeCluster: "default",
      discovery,
      selectedNamespaces: ["kube-system"],
      rows: new Map(),
      selected: null,
      view: "overview",
    });
  });

  /** The watch lists nothing at subscribe time, then delivers rows as deltas. */
  function coldWatch() {
    let emit: (b: WatchBatch) => void = () => {};
    startWatch.mockImplementation(async (_cluster, _request, onBatch) => {
      emit = onBatch as (b: WatchBatch) => void;
      const handle: WatchHandle = {
        subscriptionId: 1,
        spec: { columns: [], namespaced: true },
        initial: batch({ snapshot: true }),
      };
      return { handle, stop: vi.fn() };
    });
    return () => emit;
  }

  it("selects the object once a cold watch delivers it", async () => {
    const emitter = coldWatch();
    await useStore.getState().openObject("core/v1/pods", "production", row.name);

    // Nothing has listed yet — this is where the selection used to be dropped.
    expect(useStore.getState().selected).toBeNull();
    expect(useStore.getState().view).toBe("resources");

    emitter()(batch({ upserts: [row], state: { state: "ready" } }));
    expect(useStore.getState().selected?.uid).toBe("uid-1");
  });

  it("widens the namespace filter so the object is in scope", async () => {
    coldWatch();
    await useStore.getState().openObject("core/v1/pods", "production", row.name);
    expect(useStore.getState().selectedNamespaces).toEqual([]);
    expect(startWatch).toHaveBeenCalledTimes(1);
    expect(startWatch.mock.calls[0]?.[1]).toMatchObject({
      resource: "core/v1/pods",
      namespace: null,
    });
  });

  it("selects immediately when the watch already listed", async () => {
    startWatch.mockImplementation(async () => ({
      handle: {
        subscriptionId: 2,
        spec: { columns: [], namespaced: true },
        initial: batch({ snapshot: true, upserts: [row], state: { state: "ready" } }),
      } satisfies WatchHandle,
      stop: vi.fn(),
    }));
    await useStore.getState().openObject("core/v1/pods", "production", row.name);
    expect(useStore.getState().selected?.uid).toBe("uid-1");
  });

  it("gives up once the list is complete without the object", async () => {
    const emitter = coldWatch();
    await useStore.getState().openObject("core/v1/pods", "production", "already-gone");
    emitter()(batch({ state: { state: "ready" } }));
    // A row appearing later must not hijack the view the user moved on to.
    emitter()(batch({ upserts: [{ ...row, name: "already-gone" }], epoch: 2 }));
    expect(useStore.getState().selected).toBeNull();
  });

  it("reports a resource the cluster does not serve", async () => {
    await useStore.getState().openObject("apps/v1/widgets", "production", "x");
    expect(useStore.getState().error).toContain("apps/v1/widgets");
    expect(startWatch).not.toHaveBeenCalled();
  });
});
