import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BulkBar } from "./BulkBar";
import type { BulkOutcome, ExportResult, ResourceDescriptor, Row, TargetRef } from "../types";

const deleteObjects =
  vi.fn<(c: string, t: TargetRef[], confirmation: string) => Promise<BulkOutcome[]>>();
const restartWorkloads = vi.fn<(c: string, t: TargetRef[]) => Promise<BulkOutcome[]>>();
const exportObjects = vi.fn<(c: string, t: TargetRef[]) => Promise<ExportResult>>();

vi.mock("../api", () => ({
  api: {
    deleteObjects: (c: string, t: TargetRef[], k: string) => deleteObjects(c, t, k),
    restartWorkloads: (c: string, t: TargetRef[]) => restartWorkloads(c, t),
    exportObjects: (c: string, t: TargetRef[]) => exportObjects(c, t),
  },
}));

const descriptor = (over: Partial<ResourceDescriptor> = {}): ResourceDescriptor => ({
  key: "apps/v1/deployments",
  group: "apps",
  version: "v1",
  kind: "Deployment",
  plural: "deployments",
  apiVersion: "apps/v1",
  namespaced: true,
  verbs: ["create", "get", "list", "delete", "patch"],
  shortNames: ["deploy"],
  isCrd: false,
  printerColumns: [],
  watchable: true,
  editable: true,
  deletable: true,
  ...over,
});

const row = (name: string): Row => ({
  uid: `uid-${name}`,
  name,
  namespace: "app",
  cells: [],
  created: null,
  resourceVersion: "1",
  health: "ok",
  terminating: false,
});

function mount(rows: Row[], over: Partial<ResourceDescriptor> = {}) {
  const onClear = vi.fn();
  render(
    <BulkBar
      cluster="default"
      resource={descriptor(over)}
      selected={rows}
      visible={[row("web"), row("api"), row("worker")]}
      onClear={onClear}
      onDone={vi.fn()}
    />,
  );
  return onClear;
}

const button = (label: string) =>
  screen.getAllByRole("button").find((b) => b.textContent === label) as HTMLButtonElement;

afterEach(() => {
  cleanup();
  deleteObjects.mockReset();
  restartWorkloads.mockReset();
  exportObjects.mockReset();
});

describe("BulkBar", () => {
  it("offers nothing destructive until rows are selected", () => {
    mount([]);
    expect(button("Delete")).toBeUndefined();
    expect(button("Restart")).toBeUndefined();
    // Exporting the whole list needs no selection.
    expect(button("Download all (3)")).toBeTruthy();
  });

  it("will not delete until the size of the set is typed", async () => {
    mount([row("web"), row("api")]);
    fireEvent.click(button("Delete"));

    const confirm = screen.getAllByRole("button").find((b) => b.textContent === "Delete" && b.classList.contains("button--danger") && b.closest(".modal")) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);

    // One object's name is not a confirmation for two objects.
    fireEvent.change(screen.getByLabelText(/to confirm/), { target: { value: "web" } });
    expect(confirm.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText(/to confirm/), { target: { value: "2" } });
    expect(confirm.disabled).toBe(false);

    deleteObjects.mockResolvedValue([
      { resource: "apps/v1/deployments", namespace: "app", name: "web", ok: true, error: null },
      { resource: "apps/v1/deployments", namespace: "app", name: "api", ok: true, error: null },
    ]);
    fireEvent.click(confirm);
    await waitFor(() => expect(deleteObjects).toHaveBeenCalledWith("default", expect.any(Array), "2"));
  });

  it("lists every name in the confirmation, so the number can be checked", () => {
    mount([row("web"), row("api")]);
    fireEvent.click(button("Delete"));
    const list = document.querySelector(".bulk__list");
    expect(list?.textContent).toContain("web");
    expect(list?.textContent).toContain("api");
  });

  it("reports the failures and not the successes", async () => {
    mount([row("web"), row("api")]);
    restartWorkloads.mockResolvedValue([
      { resource: "apps/v1/deployments", namespace: "app", name: "web", ok: true, error: null },
      { resource: "apps/v1/deployments", namespace: "app", name: "api", ok: false, error: "forbidden" },
    ]);

    fireEvent.click(button("Restart"));
    expect(await screen.findByText(/1 of 2 restarted/)).toBeTruthy();
    // A wall of successes would bury the one that matters.
    const failures = document.querySelector(".bulk__failures");
    expect(failures?.textContent).toContain("forbidden");
    expect(failures?.textContent).not.toContain("web");
  });

  it("says when an export was capped rather than letting it look complete", async () => {
    mount([row("web")]);
    exportObjects.mockResolvedValue({
      yaml: "apiVersion: v1\n",
      exported: 500,
      failed: [],
      truncated: true,
    });
    // jsdom has no object URLs.
    URL.createObjectURL = vi.fn(() => "blob:x");
    URL.revokeObjectURL = vi.fn();

    fireEvent.click(button("Download YAML"));
    expect(await screen.findByText(/capped at the export limit/)).toBeTruthy();
  });

  it("hides restart for a kind that has no pod template", () => {
    mount([row("cm")], { kind: "ConfigMap", plural: "configmaps", key: "core/v1/configmaps" });
    expect(button("Restart")).toBeUndefined();
    expect(button("Delete")).toBeTruthy();
  });

  it("refuses delete when the cluster does not grant it", () => {
    mount([row("web")], { deletable: false });
    expect(button("Delete").disabled).toBe(true);
  });
});
