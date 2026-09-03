import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BulkBar } from "./BulkBar";
import type { BulkOutcome, ExportResult, ResourceDescriptor, Row, TargetRef } from "../types";

const deleteObjects =
  vi.fn<(c: string, t: TargetRef[], confirmation: string) => Promise<BulkOutcome[]>>();
const restartWorkloads = vi.fn<(c: string, t: TargetRef[]) => Promise<BulkOutcome[]>>();
const exportObjectsToFile =
  vi.fn<(c: string, t: TargetRef[], path: string) => Promise<ExportResult>>();
const save = vi.fn<() => Promise<string | null>>();

vi.mock("@tauri-apps/plugin-dialog", () => ({ save: () => save() }));

vi.mock("../api", () => ({
  api: {
    deleteObjects: (c: string, t: TargetRef[], k: string) => deleteObjects(c, t, k),
    restartWorkloads: (c: string, t: TargetRef[]) => restartWorkloads(c, t),
    exportObjectsToFile: (c: string, t: TargetRef[], p: string) => exportObjectsToFile(c, t, p),
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
  exportObjectsToFile.mockReset();
  save.mockReset();
});

describe("BulkBar", () => {
  it("offers nothing destructive until rows are selected", () => {
    mount([]);
    expect(button("Delete")).toBeUndefined();
    expect(button("Restart")).toBeUndefined();
    // Exporting the whole list needs no selection.
    expect(button("Export all (3)…")).toBeTruthy();
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
    save.mockResolvedValue("/tmp/deployments.zip");
    exportObjectsToFile.mockResolvedValue({
      yaml: "",
      exported: 500,
      failed: [],
      truncated: true,
    });

    fireEvent.click(button("Export YAML…"));
    expect(await screen.findByText(/capped at 500/)).toBeTruthy();
    expect(exportObjectsToFile).toHaveBeenCalledWith(
      "default",
      expect.any(Array),
      "/tmp/deployments.zip",
    );
  });

  it("treats a cancelled save dialog as an answer, not a failure", async () => {
    mount([row("web")]);
    save.mockResolvedValue(null);

    fireEvent.click(button("Export YAML…"));
    await waitFor(() => expect(save).toHaveBeenCalled());
    // Nothing is written and nothing is reported as wrong.
    expect(exportObjectsToFile).not.toHaveBeenCalled();
    expect(document.querySelector(".error")).toBeNull();
  });

  it("writes where the dialog said, and says so", async () => {
    mount([row("web"), row("api")]);
    save.mockResolvedValue("/Users/me/Desktop/app.zip");
    exportObjectsToFile.mockResolvedValue({
      yaml: "",
      exported: 2,
      failed: [],
      truncated: false,
    });

    fireEvent.click(button("Export YAML…"));
    // The path matters: a file written somewhere the user cannot find is a
    // file they will assume was never written.
    expect(await screen.findByText(/2 object\(s\) written to \/Users\/me\/Desktop\/app.zip/)).toBeTruthy();
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
