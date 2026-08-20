import { create } from "zustand";
import { api, startWatch } from "./api";
import type {
  ClusterStatus,
  Preferences,
  ClusterSummary,
  ContextEntry,
  DiscoveryCache,
  ResourceDescriptor,
  Row,
  TableSpec,
  WatchBatch,
  WatchState,
} from "./types";

interface ActiveWatch {
  subscriptionId: number;
  stop: () => void;
  /** Namespace this watch covers; `null` means all namespaces. */
  scope: string | null;
  epoch: number;
}

interface AppState {
  contexts: ContextEntry[];
  clusters: Record<string, ClusterSummary>;
  activeCluster: string | null;
  connecting: string | null;

  discovery: DiscoveryCache | null;
  /** Every namespace visible in this cluster. */
  namespaces: string[];
  /** Selected namespaces. Empty means all namespaces. */
  selectedNamespaces: string[];

  resource: ResourceDescriptor | null;
  spec: TableSpec | null;
  rows: Map<string, Row>;
  watchState: WatchState;
  filter: string;

  selected: Row | null;
  error: string | null;
  /** Which main view is showing. */
  view: "overview" | "resources" | "helmReleases" | "helmRepos" | "security" | "gitops" | "settings";
  preferences: Preferences | null;
  /** Node scope for the overview dashboard. */
  overviewScope: "all" | "controlPlane" | "workers";
  /** Bottom dock with active port forwards. */
  forwardsOpen: boolean;
  paletteOpen: boolean;

  loadContexts: () => Promise<void>;
  setContexts: (contexts: ContextEntry[]) => void;
  connect: (context: string) => Promise<void>;
  disconnect: (cluster: string) => Promise<void>;
  applyStatus: (cluster: string, status: ClusterStatus) => void;
  selectResource: (resource: ResourceDescriptor) => Promise<void>;
  setNamespaces: (namespaces: string[]) => Promise<void>;
  setFilter: (filter: string) => void;
  select: (row: Row | null) => void;
  refreshDiscovery: () => Promise<void>;
  dismissError: () => void;
  showForwards: () => void;
  toggleForwards: () => void;
  setPaletteOpen: (open: boolean) => void;
  showOverview: () => void;
  showView: (
    view:
      | "overview"
      | "resources"
      | "helmReleases"
      | "helmRepos"
      | "security"
      | "gitops"
      | "settings",
  ) => void;
  loadPreferences: () => Promise<void>;
  savePreferences: (preferences: Preferences) => Promise<void>;
  /** Navigate to another object by resource key, name and namespace. */
  openObject: (resource: string, namespace: string | null, name: string) => Promise<void>;
  setOverviewScope: (scope: "all" | "controlPlane" | "workers") => void;
}

/** The theme is a document attribute so CSS alone decides every colour. */
function applyTheme(theme: Preferences["theme"]) {
  document.documentElement.dataset.theme = theme;
}

let activeWatches: ActiveWatch[] = [];
/** Row to select once the watch lists it, set by `openObject`. */
let pendingSelection: { resource: string; namespace: string | null; name: string } | null = null;

/**
 * Take a pending navigation once its row shows up.
 *
 * Navigating to a resource nobody was watching yet gets an empty initial
 * snapshot — the reflector has not listed. So the request has to survive that
 * first batch and be retried as deltas arrive, or the table switches without
 * ever opening the object. It is dropped once the scope that would contain the
 * object has listed, so a row that never appears cannot hijack a later
 * navigation.
 */
function takePendingRow(
  resource: string,
  rows: Map<string, Row>,
  state: WatchState,
  scope: string | null,
): Row | null {
  const pending = pendingSelection;
  if (!pending || pending.resource !== resource) return null;

  const row =
    [...rows.values()].find(
      (candidate) =>
        candidate.name === pending.name &&
        (pending.namespace === null || candidate.namespace === pending.namespace),
    ) ?? null;

  const covered = scope === null || scope === pending.namespace;
  if (row || (covered && state.state === "ready")) pendingSelection = null;
  return row;
}

function stopActiveWatches() {
  for (const watch of activeWatches) watch.stop();
  activeWatches = [];
}

/**
 * Fold a batch into the row map.
 *
 * With several namespaces selected there is one watch per namespace, so a
 * snapshot must replace only the rows belonging to that watch's scope —
 * clearing the whole map would wipe the other namespaces every time one of them
 * re-lists.
 */
function applyBatch(
  current: Map<string, Row>,
  batch: WatchBatch,
  scope: string | null,
): Map<string, Row> {
  if (batch.snapshot) {
    const next = new Map(current);
    for (const [uid, row] of current) {
      if (scope === null || row.namespace === scope) next.delete(uid);
    }
    for (const row of batch.upserts) next.set(row.uid, row);
    return next;
  }
  if (batch.upserts.length === 0 && batch.deletes.length === 0) {
    return current;
  }
  const next = new Map(current);
  for (const row of batch.upserts) next.set(row.uid, row);
  for (const uid of batch.deletes) next.delete(uid);
  return next;
}

export const useStore = create<AppState>((set, get) => ({
  contexts: [],
  clusters: {},
  activeCluster: null,
  connecting: null,

  discovery: null,
  namespaces: [],
  selectedNamespaces: [],

  resource: null,
  spec: null,
  rows: new Map(),
  watchState: { state: "initializing" },
  filter: "",

  selected: null,
  error: null,
  view: "overview",
  preferences: null,
  overviewScope: "workers",
  forwardsOpen: false,
  paletteOpen: false,

  setContexts: (contexts) => set({ contexts }),

  loadContexts: async () => {
    try {
      set({ contexts: await api.listContexts() });
    } catch (err) {
      set({ error: String(err) });
    }
  },

  connect: async (context) => {
    if (get().connecting) return;
    set({ connecting: context, error: null });
    try {
      const summary = await api.connectCluster(context);
      const [discovery, namespaces] = await Promise.all([
        api.discover(context),
        api.listNamespaces(context),
      ]);
      stopActiveWatches();
      set((s) => ({
        clusters: { ...s.clusters, [summary.id]: summary },
        activeCluster: summary.id,
        discovery,
        namespaces,
        selectedNamespaces: [],
        resource: null,
        spec: null,
        rows: new Map(),
        selected: null,
        connecting: null,
      }));
    } catch (err) {
      set({ error: String(err), connecting: null });
    }
  },

  disconnect: async (cluster) => {
    stopActiveWatches();
    await api.disconnectCluster(cluster);
    set((s) => {
      const clusters = { ...s.clusters };
      delete clusters[cluster];
      const wasActive = s.activeCluster === cluster;
      return {
        clusters,
        activeCluster: wasActive ? null : s.activeCluster,
        discovery: wasActive ? null : s.discovery,
        resource: wasActive ? null : s.resource,
        rows: wasActive ? new Map() : s.rows,
        selected: wasActive ? null : s.selected,
      };
    });
  },

  applyStatus: (cluster, status) =>
    set((s) => {
      const existing = s.clusters[cluster];
      if (!existing) return s;
      return { clusters: { ...s.clusters, [cluster]: { ...existing, status } } };
    }),

  selectResource: async (resource) => {
    const cluster = get().activeCluster;
    if (!cluster) return;

    stopActiveWatches();
    set({
      resource,
      spec: null,
      rows: new Map(),
      selected: null,
      view: "resources",
      watchState: { state: "initializing" },
      error: null,
    });

    // A cluster-scoped resource ignores the namespace selection; an empty
    // selection means all namespaces, which is one watch, not none.
    const scopes: (string | null)[] =
      resource.namespaced && get().selectedNamespaces.length > 0
        ? get().selectedNamespaces
        : [null];

    try {
      const started = await Promise.all(
        scopes.map(async (scope) => {
          const { handle, stop } = await startWatch(
            cluster,
            {
              resource: resource.key,
              namespace: scope,
              labelSelector: null,
              fieldSelector: null,
            },
            (batch) => {
              const watch = activeWatches.find((w) => w.subscriptionId === handle.subscriptionId);
              // Deltas from a previous epoch describe a list that has already
              // been replaced by a re-list.
              if (watch && batch.epoch < watch.epoch) return;
              if (watch) watch.epoch = batch.epoch;
              set((s) => {
                const rows = applyBatch(s.rows, batch, scope);
                const pending = takePendingRow(resource.key, rows, batch.state, scope);
                return {
                  rows,
                  watchState: batch.state,
                  ...(pending ? { selected: pending } : {}),
                };
              });
            },
          );
          return { handle, stop, scope };
        }),
      );

      // The selection may have changed while we awaited.
      if (get().resource?.key !== resource.key) {
        for (const entry of started) entry.stop();
        return;
      }

      activeWatches = started.map((entry) => ({
        subscriptionId: entry.handle.subscriptionId,
        stop: entry.stop,
        scope: entry.scope,
        epoch: entry.handle.initial.epoch,
      }));

      let rows = new Map<string, Row>();
      for (const entry of started) {
        rows = applyBatch(rows, entry.handle.initial, entry.scope);
      }
      // Honour a navigation request from the overview, the related-resources
      // panel or the security centre. An empty snapshot leaves it pending for
      // the delta batches that follow.
      const state = started[0]?.handle.initial.state ?? { state: "ready" };
      const selectedRow = takePendingRow(
        resource.key,
        rows,
        state,
        started.length === 1 ? (started[0]?.scope ?? null) : null,
      );

      set({
        spec: started[0]?.handle.spec ?? null,
        rows,
        watchState: state,
        ...(selectedRow ? { selected: selectedRow } : {}),
      });
    } catch (err) {
      set({ error: String(err), watchState: { state: "error", message: String(err) } });
    }
  },

  setNamespaces: async (namespaces) => {
    set({ selectedNamespaces: namespaces });
    const resource = get().resource;
    if (resource?.namespaced) {
      await get().selectResource(resource);
    }
  },

  setFilter: (filter) => set({ filter }),
  select: (row) => set({ selected: row }),

  refreshDiscovery: async () => {
    const cluster = get().activeCluster;
    if (!cluster) return;
    try {
      set({ discovery: await api.discover(cluster, true) });
    } catch (err) {
      set({ error: String(err) });
    }
  },

  dismissError: () => set({ error: null }),
  showForwards: () => set({ forwardsOpen: true }),
  toggleForwards: () => set((s) => ({ forwardsOpen: !s.forwardsOpen })),
  setPaletteOpen: (open) => set({ paletteOpen: open }),
  showOverview: () => set({ view: "overview", selected: null }),
  showView: (view) => set({ view, selected: null }),

  loadPreferences: async () => {
    try {
      const preferences = await api.getPreferences();
      set({ preferences });
      applyTheme(preferences.theme);
    } catch (err) {
      set({ error: String(err) });
    }
  },

  savePreferences: async (preferences) => {
    const saved = await api.setPreferences(preferences);
    set({ preferences: saved });
    applyTheme(saved.theme);
  },

  openObject: async (resourceKey, namespace, name) => {
    const discovery = get().discovery;
    if (!discovery || !resourceKey) return;

    const descriptor = discovery.groups
      .flatMap((group) => group.resources)
      .find((entry) => entry.key === resourceKey);
    if (!descriptor) {
      set({ error: `This cluster has no ${resourceKey}` });
      return;
    }

    // The row only exists once the watch has listed; remember what to select
    // and let `selectResource` pick it up when rows arrive.
    pendingSelection = { resource: resourceKey, namespace, name };
    if (namespace && descriptor.namespaced && !get().selectedNamespaces.includes(namespace)) {
      // Widen the namespace filter rather than showing an empty table.
      set({ selectedNamespaces: [] });
    }
    await get().selectResource(descriptor);
  },
  setOverviewScope: (scope) => set({ overviewScope: scope }),
}));
