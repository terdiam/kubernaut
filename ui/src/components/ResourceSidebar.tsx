import { useMemo, useState } from "react";
import { useStore } from "../store";
import { CATEGORIES, categoryOf, sortWithinCategory } from "../categories";
import { Icon, type IconName } from "./Icon";
import type { ResourceDescriptor } from "../types";

interface Bucket {
  id: string;
  label: string;
  icon: IconName;
  section: "cluster" | "resources";
  standalone: boolean;
  resources: ResourceDescriptor[];
  /** Custom Resources are split further by API group, as in Lens. */
  subgroups: { name: string; resources: ResourceDescriptor[] }[];
}

/** Views that are not backed by a Kubernetes kind. */
const TOOLS: { view: "helmReleases" | "gitops" | "security"; label: string; icon: IconName }[] = [
  { view: "helmReleases", label: "Helm", icon: "helm" },
  { view: "gitops", label: "GitOps", icon: "gitops" },
  { view: "security", label: "Security Center", icon: "security" },
];

export function ResourceSidebar() {
  const discovery = useStore((s) => s.discovery);
  const resource = useStore((s) => s.resource);
  const view = useStore((s) => s.view);
  const selectResource = useStore((s) => s.selectResource);
  const refreshDiscovery = useStore((s) => s.refreshDiscovery);
  const showOverview = useStore((s) => s.showOverview);
  const showView = useStore((s) => s.showView);

  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(["config", "access", "custom", "other"]),
  );
  const [openGroups, setOpenGroups] = useState<Set<string>>(new Set());

  const buckets = useMemo<Bucket[]>(() => {
    if (!discovery) return [];
    const needle = query.trim().toLowerCase();

    const byCategory = new Map<string, ResourceDescriptor[]>();
    for (const group of discovery.groups) {
      for (const item of group.resources) {
        // Only the preferred version: older versions of the same kind are
        // noise when browsing, and stay reachable from the detail pane.
        if (!item.watchable || item.version !== group.preferredVersion) continue;
        if (
          needle &&
          !item.kind.toLowerCase().includes(needle) &&
          !item.plural.toLowerCase().includes(needle) &&
          !item.group.toLowerCase().includes(needle)
        ) {
          continue;
        }
        const id = categoryOf(item);
        byCategory.set(id, [...(byCategory.get(id) ?? []), item]);
      }
    }

    return CATEGORIES.flatMap((category) => {
      const items = byCategory.get(category.id) ?? [];
      if (items.length === 0) return [];
      items.sort((a, b) => sortWithinCategory(category.id, a, b));

      const subgroups: Bucket["subgroups"] = [];
      if (category.id === "custom" || category.id === "other") {
        const grouped = new Map<string, ResourceDescriptor[]>();
        for (const item of items) {
          const key = item.group || "core";
          grouped.set(key, [...(grouped.get(key) ?? []), item]);
        }
        for (const [name, resources] of [...grouped].sort((a, b) => a[0].localeCompare(b[0]))) {
          subgroups.push({ name, resources });
        }
      }

      return [
        {
          id: category.id,
          label: category.label,
          icon: category.icon,
          section: category.section,
          // A standalone category with exactly one kind is a link, not a group.
          standalone: category.standalone === true && items.length === 1,
          resources: items,
          subgroups,
        },
      ];
    });
  }, [discovery, query]);

  const toggle = (id: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const toggleGroup = (key: string) =>
    setOpenGroups((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  // Two API groups can ship the same kind (core `Event` and
  // `events.k8s.io/Event`); qualify the label only when it would be ambiguous.
  const ambiguousKinds = (resources: ResourceDescriptor[]) => {
    const seen = new Map<string, number>();
    for (const entry of resources) seen.set(entry.kind, (seen.get(entry.kind) ?? 0) + 1);
    return new Set([...seen].filter(([, count]) => count > 1).map(([kind]) => kind));
  };

  const item = (entry: ResourceDescriptor, indent: boolean, ambiguous: Set<string>) => (
    <li key={entry.key}>
      <button
        className={`tree-item${indent ? " tree-item--nested" : ""}${
          view === "resources" && resource?.key === entry.key ? " tree-item--active" : ""
        }`}
        onClick={() => void selectResource(entry)}
        title={`${entry.apiVersion} ${entry.kind}`}
      >
        {entry.kind}
        {ambiguous.has(entry.kind) && (
          <span className="tree-item__group">{entry.group || "core"}</span>
        )}
      </button>
    </li>
  );

  const bucketNode = (bucket: Bucket) => {
    const isCollapsed = collapsed.has(bucket.id) && !query;

    if (bucket.standalone) {
      const only = bucket.resources[0]!;
      return (
        <button
          key={bucket.id}
          className={`nav-item${
            view === "resources" && resource?.key === only.key ? " nav-item--active" : ""
          }`}
          onClick={() => void selectResource(only)}
        >
          <span className="nav-item__caret" />
          <Icon name={bucket.icon} />
          {bucket.label}
        </button>
      );
    }

    const ambiguous = ambiguousKinds(bucket.resources);

    return (
      <section key={bucket.id} className="tree-group">
        <button className="nav-item" onClick={() => toggle(bucket.id)} aria-expanded={!isCollapsed}>
          <span className={`nav-item__caret${isCollapsed ? "" : " nav-item__caret--open"}`}>▸</span>
          <Icon name={bucket.icon} />
          {bucket.label}
          <span className="tree-group__count">{bucket.resources.length}</span>
        </button>

        {!isCollapsed && bucket.subgroups.length === 0 && (
          <ul>{bucket.resources.map((entry) => item(entry, false, ambiguous))}</ul>
        )}

        {!isCollapsed &&
          bucket.subgroups.map((subgroup) => {
            const key = `${bucket.id}/${subgroup.name}`;
            const open = openGroups.has(key) || Boolean(query);
            return (
              <div key={key} className="tree-subgroup">
                <button className="tree-subgroup__head" onClick={() => toggleGroup(key)}>
                  <span className={`nav-item__caret${open ? " nav-item__caret--open" : ""}`}>▸</span>
                  {subgroup.name}
                  <span className="tree-group__count">{subgroup.resources.length}</span>
                </button>
                {open && (
                  <ul>{subgroup.resources.map((entry) => item(entry, true, ambiguous))}</ul>
                )}
              </div>
            );
          })}
      </section>
    );
  };

  return (
    <aside className="sidebar">
      <div className="sidebar__search">
        <input
          value={query}
          placeholder="Filter resources"
          onChange={(e) => setQuery(e.target.value)}
        />
        <button
          className="icon-button"
          title="Re-run discovery (picks up new CRDs)"
          onClick={() => void refreshDiscovery()}
        >
          ⟳
        </button>
      </div>

      {discovery && !discovery.crdMetadataAvailable && (
        <p className="sidebar__note">
          No permission to read CRDs — custom columns fall back to defaults.
        </p>
      )}

      <div className="sidebar__tree">
        <button
          className={`nav-item${view === "overview" ? " nav-item--active" : ""}`}
          onClick={showOverview}
        >
          <span className="nav-item__caret" />
          <Icon name="overview" />
          Overview
        </button>

        {buckets.filter((bucket) => bucket.section === "cluster").map(bucketNode)}

        {buckets.some((bucket) => bucket.section === "resources") && (
          <p className="sidebar__section">Resources</p>
        )}
        {buckets.filter((bucket) => bucket.section === "resources").map(bucketNode)}

        <p className="sidebar__section">Tools</p>
        {TOOLS.map((tool) => (
          <button
            key={tool.view}
            className={`nav-item${
              view === tool.view || (tool.view === "helmReleases" && view === "helmRepos")
                ? " nav-item--active"
                : ""
            }`}
            onClick={() => showView(tool.view)}
          >
            <span className="nav-item__caret" />
            <Icon name={tool.icon} />
            {tool.label}
          </button>
        ))}

        {(view === "helmReleases" || view === "helmRepos") && (
          <ul>
            <li>
              <button
                className={`tree-item${view === "helmReleases" ? " tree-item--active" : ""}`}
                onClick={() => showView("helmReleases")}
              >
                Releases
              </button>
            </li>
            <li>
              <button
                className={`tree-item${view === "helmRepos" ? " tree-item--active" : ""}`}
                onClick={() => showView("helmRepos")}
              >
                Repositories
              </button>
            </li>
          </ul>
        )}

        {!discovery && (
          <p className="muted sidebar__empty">Pick a cluster to see its resources.</p>
        )}
      </div>

      <button
        className={`nav-item sidebar__settings${view === "settings" ? " nav-item--active" : ""}`}
        onClick={() => showView("settings")}
      >
        <span className="nav-item__caret" />
        <Icon name="settings" />
        Settings
      </button>
    </aside>
  );
}
