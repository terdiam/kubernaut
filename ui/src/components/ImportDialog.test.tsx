import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ImportDialog } from "./ImportDialog";
import type { DocResult, ManifestPlan } from "../types";

let text = "";
const planManifest = vi.fn<() => Promise<ManifestPlan>>();
const applyManifest = vi.fn<() => Promise<DocResult[]>>();

vi.mock("../api", () => ({
  api: { planManifest: () => planManifest(), applyManifest: () => applyManifest() },
}));
vi.mock("../store", () => ({
  useStore: (select: (s: Record<string, unknown>) => unknown) =>
    select({ activeCluster: "default", selectedNamespaces: ["app"], namespaces: ["app"] }),
}));
vi.mock("../monaco", () => {
  const buffer = {
    getValue: () => text,
    setValue: (value: string) => {
      text = value;
    },
    dispose: () => {},
  };
  return {
    editorTheme: () => "kubernaut-dark",
    modelUriFor: () => ({ toString: () => "u" }),
    monaco: {
      editor: {
        getModel: () => null,
        createModel: () => buffer,
        setTheme: () => {},
        create: () => buffer,
      },
    },
  };
});

const doc = (over: Partial<ManifestPlan["docs"][0]> = {}): ManifestPlan["docs"][0] => ({
  index: 0,
  apiVersion: "apps/v1",
  kind: "Deployment",
  name: "web",
  namespace: "app",
  resource: "apps/v1/deployments",
  action: "create",
  unified: "",
  conflicts: [],
  warnings: [],
  error: null,
  ...over,
});

afterEach(() => {
  cleanup();
  text = "";
  planManifest.mockReset();
  applyManifest.mockReset();
});

const button = (label: string) =>
  screen.getAllByRole("button").find((b) => b.textContent === label) as HTMLButtonElement;

async function mount() {
  render(<ImportDialog onClose={vi.fn()} />);
  await waitFor(() => expect(document.querySelector(".manifest__editor")).toBeTruthy());
}

describe("ImportDialog", () => {
  it("does no creating of its own — no templates here", async () => {
    await mount();
    // Templates belong to the create flow; offering them here is what made the
    // two jobs hard to tell apart.
    expect(screen.queryByLabelText("Insert a template")).toBeNull();
    expect(screen.getByText("Choose a file…")).toBeTruthy();
  });

  it("applies the file's contents, not a rewritten copy", async () => {
    await mount();
    const file = new File(["apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n"], "cm.yaml");
    const input = document.querySelector(".manifest__file input") as HTMLInputElement;
    Object.defineProperty(input, "files", { value: [file] });
    fireEvent.change(input);

    await waitFor(() => expect(text).toContain("kind: ConfigMap"));
    expect(await screen.findByText("cm.yaml")).toBeTruthy();
  });

  it("will not apply until the cluster has been asked, and not while blocked", async () => {
    await mount();
    expect(button("Apply").disabled).toBe(true);

    planManifest.mockResolvedValue({ docs: [doc(), doc({ index: 1, action: "error", error: "no such kind" })] });
    fireEvent.click(button("Preview"));
    await screen.findByText("no such kind");
    expect(button("Apply").disabled).toBe(true);

    planManifest.mockResolvedValue({ docs: [doc()] });
    fireEvent.click(button("Preview"));
    await waitFor(() => expect(button("Apply").disabled).toBe(false));
  });

  it("reports a partial failure rather than closing quietly", async () => {
    await mount();
    planManifest.mockResolvedValue({ docs: [doc()] });
    fireEvent.click(button("Preview"));
    await waitFor(() => expect(button("Apply").disabled).toBe(false));

    applyManifest.mockResolvedValue([
      { index: 0, kind: "Deployment", name: "web", namespace: "app", status: "created", conflicts: [], error: null },
      { index: 1, kind: "Service", name: "web", namespace: "app", status: "error", conflicts: [], error: "denied" },
    ]);
    fireEvent.click(button("Apply"));

    expect(await screen.findByText(/1 of 2 did not go through/)).toBeTruthy();
    expect(screen.getByText("denied")).toBeTruthy();
  });
});
