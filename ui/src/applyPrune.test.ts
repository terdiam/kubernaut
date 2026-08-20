import { describe, expect, it } from "vitest";
import { prunedApply } from "./applyPrune";

const deployment = () => ({
  apiVersion: "apps/v1",
  kind: "Deployment",
  metadata: { name: "backend", namespace: "production", labels: { app: "backend" } },
  spec: {
    replicas: 2,
    template: {
      spec: {
        containers: [
          {
            name: "backend",
            image: "registry.example/backend:1.4.2",
            ports: [{ containerPort: 8080, protocol: "TCP" }],
            env: [{ name: "LOG_LEVEL", value: "info" }],
            resources: { requests: { cpu: "100m" } },
          },
          { name: "sidecar", image: "registry.example/proxy:2.1" },
        ],
      },
    },
  },
});

/** Read a path like `spec.template.spec.containers`. */
const at = (obj: unknown, path: string) =>
  path.split(".").reduce<unknown>((acc, key) => (acc as Record<string, unknown>)?.[key], obj);

describe("prunedApply", () => {
  it("sends nothing when nothing changed", () => {
    expect(prunedApply(deployment(), deployment())).toBeNull();
  });

  it("keeps identity so the apiserver can find the object", () => {
    const draft = deployment();
    draft.spec.replicas = 5;
    const doc = prunedApply(deployment(), draft)!;
    expect(doc).toMatchObject({
      apiVersion: "apps/v1",
      kind: "Deployment",
      metadata: { name: "backend", namespace: "production" },
      spec: { replicas: 5 },
    });
  });

  it("leaves untouched fields out, so their owner keeps them", () => {
    // The reported bug: editing replicas claimed `.image`, which rancher owns.
    const draft = deployment();
    draft.spec.replicas = 5;
    const doc = prunedApply(deployment(), draft)!;
    expect(at(doc, "spec.template")).toBeUndefined();
    expect(JSON.stringify(doc)).not.toContain("image");
  });

  it("carries the list key of an entry it edits", () => {
    const draft = deployment();
    draft.spec.template.spec.containers[0]!.resources = { requests: { cpu: "250m" } };
    const doc = prunedApply(deployment(), draft)!;

    const containers = at(doc, "spec.template.spec.containers") as Record<string, unknown>[];
    // Only the edited container, identified by name, and only the edited field.
    expect(containers).toHaveLength(1);
    expect(containers[0]).toEqual({ name: "backend", resources: { requests: { cpu: "250m" } } });
  });

  it("still claims a field the user really did edit", () => {
    const draft = deployment();
    draft.spec.template.spec.containers[0]!.image = "registry.example/backend:1.5.0";
    const doc = prunedApply(deployment(), draft)!;
    const containers = at(doc, "spec.template.spec.containers") as Record<string, unknown>[];
    expect(containers[0]).toEqual({ name: "backend", image: "registry.example/backend:1.5.0" });
  });

  it("edits an env entry without claiming its neighbours", () => {
    const draft = deployment();
    draft.spec.template.spec.containers[0]!.env = [{ name: "LOG_LEVEL", value: "debug" }];
    const doc = prunedApply(deployment(), draft)!;
    const containers = at(doc, "spec.template.spec.containers") as Record<string, unknown>[];
    expect(containers[0]).toEqual({
      name: "backend",
      env: [{ name: "LOG_LEVEL", value: "debug" }],
    });
  });

  it("replaces a whole list when an entry is removed", () => {
    // Deleting is expressed by owning the list and leaving the entry out.
    const draft = deployment();
    draft.spec.template.spec.containers = [draft.spec.template.spec.containers[0]!];
    const doc = prunedApply(deployment(), draft)!;
    const containers = at(doc, "spec.template.spec.containers") as Record<string, unknown>[];
    expect(containers).toHaveLength(1);
    expect(containers[0]).toHaveProperty("image");
  });

  it("claims the parent object when a field is removed", () => {
    const draft = deployment();
    delete (draft.metadata as { labels?: unknown }).labels;
    const doc = prunedApply(deployment(), draft)!;
    expect(doc.metadata).toMatchObject({ name: "backend", namespace: "production" });
    expect(doc.metadata).not.toHaveProperty("labels");
  });

  it("replaces a list it has no key for", () => {
    const live = { metadata: { name: "x" }, spec: { finalizers: ["a", "b"] } };
    const draft = { metadata: { name: "x" }, spec: { finalizers: ["a", "c"] } };
    const doc = prunedApply(live, draft)!;
    expect(at(doc, "spec.finalizers")).toEqual(["a", "c"]);
  });
});
