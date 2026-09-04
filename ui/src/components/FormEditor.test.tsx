import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FormEditor } from "./FormEditor";
import type { ApplyOutcome, EditRequest } from "../types";

const applyEdit = vi.fn<(cluster: string, request: EditRequest) => Promise<ApplyOutcome>>();

vi.mock("../api", () => ({
  api: {
    // Reference fields ask the cluster for their options.
    lookupOptions: () => Promise.resolve([]),
    applyEdit: (cluster: string, request: EditRequest) => applyEdit(cluster, request),
    previewEdit: vi.fn(),
  },
}));

const deployment = () => ({
  apiVersion: "apps/v1",
  kind: "Deployment",
  metadata: { name: "backend", namespace: "production" },
  spec: {
    replicas: 2,
    template: {
      spec: {
        containers: [{ name: "backend", image: "registry.example/backend:1.4.2" }],
      },
    },
  },
});

function mount() {
  return render(
    <FormEditor
      cluster="default"
      resource="apps/v1/deployments"
      group="apps"
      kind="Deployment"
      namespace="production"
      name="backend"
      initial={deployment()}
      onApplied={vi.fn()}
    />,
  );
}

/** Change the replica count, which is a plain number field. */
function editReplicas(value: string) {
  const input = screen.getByLabelText(/replicas/i);
  fireEvent.change(input, { target: { value } });
}

describe("FormEditor", () => {
  beforeEach(() => applyEdit.mockReset());
  afterEach(cleanup);

  it("applies only the field that changed", async () => {
    applyEdit.mockResolvedValue({ status: "applied", yaml: "", resourceVersion: "2" });
    mount();
    editReplicas("5");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(applyEdit).toHaveBeenCalledTimes(1);
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent).toEqual({
      apiVersion: "apps/v1",
      kind: "Deployment",
      metadata: { name: "backend", namespace: "production" },
      spec: { replicas: 5 },
    });
    // The image belongs to another manager and was not edited.
    expect(applyEdit.mock.calls[0]![1].yaml).not.toContain("image");
  });

  it("offers to take the field when its owner refuses", async () => {
    applyEdit.mockResolvedValueOnce({
      status: "conflict",
      conflicts: [
        { manager: "rancher", field: '.spec.template.spec.containers[name="backend"].image' },
      ],
    });
    mount();
    editReplicas("5");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => screen.getByText(/Owned by rancher/));

    expect(screen.getByText(/containers\[name="backend"\]\.image/)).toBeTruthy();

    applyEdit.mockResolvedValueOnce({ status: "applied", yaml: "", resourceVersion: "3" });
    fireEvent.click(screen.getByRole("button", { name: /take ownership/i }));

    expect(applyEdit).toHaveBeenCalledTimes(2);
    expect(applyEdit.mock.calls[1]![1].force).toBe(true);
  });

  it("lets a container mount a volume the pod already declares", async () => {
    applyEdit.mockResolvedValue({ status: "applied", yaml: "", resourceVersion: "2" });
    render(
      <FormEditor
        cluster="default"
        resource="apps/v1/deployments"
        group="apps"
        kind="Deployment"
        namespace="production"
        name="backend"
        initial={{
          apiVersion: "apps/v1",
          kind: "Deployment",
          metadata: { name: "backend", namespace: "production" },
          spec: {
            template: {
              spec: {
                containers: [{ name: "backend", image: "registry.example/backend:1.4.2" }],
                // Already declared, so the mount below is offered by name, not typed blind.
                volumes: [{ name: "data", persistentVolumeClaim: { claimName: "data" } }],
              },
            },
          },
        }}
        onApplied={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "+ Add mount" }));
    // The suggested volume is filled in automatically; only the path is typed.
    const volumeField = screen.getByPlaceholderText("which volume") as HTMLInputElement;
    expect(volumeField.value).toBe("data");
    fireEvent.change(screen.getByPlaceholderText("/path/in/container"), {
      target: { value: "/var/lib/app" },
    });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.template.spec.containers[0].volumeMounts).toEqual([
      { name: "data", mountPath: "/var/lib/app" },
    ]);
  });

  it("suggests a StatefulSet's own volume claim template as a mount target", async () => {
    render(
      <FormEditor
        cluster="default"
        resource="apps/v1/statefulsets"
        group="apps"
        kind="StatefulSet"
        namespace="production"
        name="db"
        initial={{
          apiVersion: "apps/v1",
          kind: "StatefulSet",
          metadata: { name: "db", namespace: "production" },
          spec: {
            serviceName: "db",
            replicas: 1,
            template: { spec: { containers: [{ name: "postgres", image: "postgres:16" }] } },
            // No `spec.volumes` entry — StatefulSet gives each template an
            // implicit volume, so the suggestion has to come from here.
            volumeClaimTemplates: [
              { metadata: { name: "data" }, spec: { resources: { requests: { storage: "10Gi" } } } },
            ],
          },
        }}
        onApplied={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "+ Add mount" }));
    const volumeField = screen.getByPlaceholderText("which volume") as HTMLInputElement;
    expect(volumeField.value).toBe("data");
  });

  it("provisions per-replica storage for a StatefulSet from a volume claim template", async () => {
    applyEdit.mockResolvedValue({ status: "applied", yaml: "", resourceVersion: "2" });
    render(
      <FormEditor
        cluster="default"
        resource="apps/v1/statefulsets"
        group="apps"
        kind="StatefulSet"
        namespace="production"
        name="db"
        initial={{
          apiVersion: "apps/v1",
          kind: "StatefulSet",
          metadata: { name: "db", namespace: "production" },
          spec: { serviceName: "db", replicas: 1 },
        }}
        onApplied={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "+ Add volume claim template" }));
    fireEvent.change(screen.getByPlaceholderText("10Gi"), { target: { value: "20Gi" } });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.volumeClaimTemplates).toEqual([
      {
        metadata: { name: "data" },
        spec: { accessModes: ["ReadWriteOnce"], resources: { requests: { storage: "20Gi" } } },
      },
    ]);
  });

  function mountBareDeployment() {
    applyEdit.mockResolvedValue({ status: "applied", yaml: "", resourceVersion: "2" });
    render(
      <FormEditor
        cluster="default"
        resource="apps/v1/deployments"
        group="apps"
        kind="Deployment"
        namespace="production"
        name="backend"
        initial={{
          apiVersion: "apps/v1",
          kind: "Deployment",
          metadata: { name: "backend", namespace: "production" },
          spec: { template: { spec: { containers: [{ name: "backend", image: "nginx" }] } } },
        }}
        onApplied={vi.fn()}
      />,
    );
  }

  it("sources an environment variable from a ConfigMap key", async () => {
    mountBareDeployment();

    fireEvent.click(screen.getByRole("button", { name: "+ Add variable" }));
    const row = document.querySelector(".kv__row") as HTMLElement;
    fireEvent.change(row.querySelector("input")!, { target: { value: "DB_HOST" } });
    fireEvent.change(row.querySelector("select")!, { target: { value: "configMap" } });

    // The ConfigMap doesn't exist in the cluster yet, so its name is typed.
    // Inputs in the row, in DOM order: the variable's own name, then the
    // reference name (now typed, not selected), then the key.
    fireEvent.click(within(row).getByText("type a name"));
    const [, nameRef, key] = row.querySelectorAll("input") as unknown as [
      HTMLInputElement,
      HTMLInputElement,
      HTMLInputElement,
    ];
    fireEvent.change(nameRef, { target: { value: "app-config" } });
    fireEvent.change(key, { target: { value: "host" } });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.template.spec.containers[0].env).toEqual([
      { name: "DB_HOST", valueFrom: { configMapKeyRef: { name: "app-config", key: "host" } } },
    ]);
  });

  it("sources an environment variable from a Secret key", async () => {
    mountBareDeployment();

    fireEvent.click(screen.getByRole("button", { name: "+ Add variable" }));
    const row = document.querySelector(".kv__row") as HTMLElement;
    fireEvent.change(row.querySelector("input")!, { target: { value: "DB_PASSWORD" } });
    fireEvent.change(row.querySelector("select")!, { target: { value: "secret" } });

    fireEvent.click(within(row).getByText("type a name"));
    const [, nameRef, key] = row.querySelectorAll("input") as unknown as [
      HTMLInputElement,
      HTMLInputElement,
      HTMLInputElement,
    ];
    fireEvent.change(nameRef, { target: { value: "db-credentials" } });
    fireEvent.change(key, { target: { value: "password" } });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.template.spec.containers[0].env).toEqual([
      {
        name: "DB_PASSWORD",
        valueFrom: { secretKeyRef: { name: "db-credentials", key: "password" } },
      },
    ]);
  });

  it("keeps a downward-API env var (fieldRef) untouched and unselectable", async () => {
    applyEdit.mockResolvedValue({ status: "applied", yaml: "", resourceVersion: "2" });
    render(
      <FormEditor
        cluster="default"
        resource="apps/v1/deployments"
        group="apps"
        kind="Deployment"
        namespace="production"
        name="backend"
        initial={{
          apiVersion: "apps/v1",
          kind: "Deployment",
          metadata: { name: "backend", namespace: "production" },
          spec: {
            template: {
              spec: {
                containers: [
                  {
                    name: "backend",
                    image: "nginx",
                    env: [{ name: "POD_IP", valueFrom: { fieldRef: { fieldPath: "status.podIP" } } }],
                  },
                ],
              },
            },
          },
        }}
        onApplied={vi.fn()}
      />,
    );

    const row = document.querySelector(".kv__row") as HTMLElement;
    // No source select for a shape this field does not model — nothing to
    // pick that could silently rewrite it.
    expect(row.querySelector("select")).toBeNull();
    expect((row.querySelectorAll("input")[1] as HTMLInputElement).disabled).toBe(true);

    // Renaming the variable is a real, in-model edit — unlike its value, which
    // this field never touches — and must carry `fieldRef` through unharmed.
    fireEvent.change(row.querySelectorAll("input")[0]!, { target: { value: "POD_IP_ADDR" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.template.spec.containers[0].env).toEqual([
      { name: "POD_IP_ADDR", valueFrom: { fieldRef: { fieldPath: "status.podIP" } } },
    ]);
  });

  it("imports every key of a ConfigMap or Secret as environment variables", async () => {
    mountBareDeployment();

    fireEvent.click(screen.getByRole("button", { name: "+ Add source" }));
    const row = document.querySelector(".kv__row") as HTMLElement;
    fireEvent.change(row.querySelector("select")!, { target: { value: "secret" } });
    fireEvent.click(within(row).getByText("type a name"));
    fireEvent.change(row.querySelectorAll("input")[0]!, { target: { value: "app-secrets" } });
    fireEvent.change(row.querySelectorAll("input")[1]!, { target: { value: "APP_" } });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const sent = JSON.parse(applyEdit.mock.calls[0]![1].yaml);
    expect(sent.spec.template.spec.containers[0].envFrom).toEqual([
      { secretRef: { name: "app-secrets" }, prefix: "APP_" },
    ]);
  });
});
