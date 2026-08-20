import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parse } from "yaml";
import { CreateDialog } from "./CreateDialog";
import type { ManifestPlan, ResourceDescriptor } from "../types";

let text = "";
let applied: { yaml: string; namespace: string | null } | null = null;

const planManifest = vi.fn<() => Promise<ManifestPlan>>();

vi.mock("../api", () => ({
  api: {
    // Reference fields ask the cluster for their options.
    lookupOptions: () => Promise.resolve([]),
    planManifest: () => planManifest(),
    applyManifest: (_c: string, yaml: string, namespace: string | null) => {
      applied = { yaml, namespace };
      return Promise.resolve([]);
    },
  },
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

const descriptor = (over: Partial<ResourceDescriptor> = {}): ResourceDescriptor => ({
  key: "apps/v1/deployments",
  group: "apps",
  version: "v1",
  kind: "Deployment",
  plural: "deployments",
  apiVersion: "apps/v1",
  namespaced: true,
  verbs: ["create", "get", "list", "patch", "delete"],
  shortNames: ["deploy"],
  isCrd: false,
  printerColumns: [],
  watchable: true,
  editable: true,
  deletable: true,
  ...over,
});

let state: Record<string, unknown> = {};
vi.mock("../store", () => ({
  useStore: (select: (s: Record<string, unknown>) => unknown) => select(state),
}));

function mount(over: Record<string, unknown> = {}) {
  state = {
    activeCluster: "default",
    resource: descriptor(),
    discovery: { groups: [{ resources: [descriptor(), descriptor({ key: "core/v1/secrets", group: "", kind: "Secret", plural: "secrets", apiVersion: "v1" })] }] },
    selectedNamespaces: ["app"],
    namespaces: ["app", "kube-system"],
    ...over,
  };
  render(<CreateDialog onClose={vi.fn()} />);
}

const button = (label: string) =>
  screen.getAllByRole("button").find((b) => b.textContent === label) as HTMLButtonElement;
/** The Form/YAML switch is a tablist, so it is not among the plain buttons. */
const tab = (label: string) => screen.getByRole("tab", { name: label }) as HTMLButtonElement;
/** The create dialog owns the name field; the edit form has none, because a
 *  name cannot be changed after the fact. */
const nameField = () => document.querySelector("#create-name") as HTMLInputElement;

afterEach(() => {
  cleanup();
  text = "";
  applied = null;
  planManifest.mockReset();
});

describe("CreateDialog", () => {
  it("creates the kind the page is showing, without being told", () => {
    mount();
    expect(screen.getByText(/Create Deployment/)).toBeTruthy();
    // The form opens first for a kind that has a layout.
    expect(document.querySelector(".create__form")).toBeTruthy();
  });

  it("offers a name field, which the edit form deliberately does not have", () => {
    mount();
    // Without this the form can only ever produce an object called `example`.
    expect(nameField()).toBeTruthy();
    expect(nameField().value).toBe("example");
  });

  it("renames the labels that still match, so the template stays wired together", async () => {
    mount();
    fireEvent.change(nameField(), { target: { value: "checkout" } });
    fireEvent.click(tab("YAML"));
    await waitFor(() => expect(text).toContain("checkout"));

    const doc = parse(text);
    // A Deployment whose selector still says `app: example` is one no Service
    // from the same template can find.
    expect(doc.metadata.labels.app).toBe("checkout");
    expect(doc.spec.selector.matchLabels.app).toBe("checkout");
    expect(doc.spec.template.metadata.labels.app).toBe("checkout");
    // Scoped to labels: nothing else that happens to hold the old name moves.
    expect(doc.spec.template.spec.containers[0].name).toBe("app");
  });

  it("leaves a label the user set deliberately alone", async () => {
    mount();
    fireEvent.change(nameField(), { target: { value: "first" } });
    fireEvent.click(tab("YAML"));
    await waitFor(() => expect(text).toContain("first"));
    // Pretend the user pinned the label to something of their own, then renamed.
    text = text.replace(/app: first/g, "app: chosen-by-hand");
    fireEvent.click(tab("Form"));
    fireEvent.change(nameField(), { target: { value: "second" } });
    fireEvent.click(tab("YAML"));
    await waitFor(() => expect(text).toContain("second"));

    expect(parse(text).metadata.labels.app).toBe("chosen-by-hand");
  });

  it("carries the form's edits into YAML rather than restarting", async () => {
    mount();
    fireEvent.change(nameField(), { target: { value: "checkout" } });

    fireEvent.click(tab("YAML"));
    await waitFor(() => expect(text).toContain("checkout"));
    // The template's other values survive the switch.
    expect(parse(text).spec.replicas).toBe(2);
  });

  it("refuses to switch back on YAML that does not parse, and says why", async () => {
    mount();
    fireEvent.click(tab("YAML"));
    await waitFor(() => expect(text.length).toBeGreaterThan(0));
    text = "just: some: bad";

    fireEvent.click(tab("Form"));
    expect(screen.getByText(/has to parse before switching/)).toBeTruthy();
    // Still in YAML, so the broken text is not silently discarded.
    expect(document.querySelector(".manifest__editor")).toBeTruthy();
  });

  it("applies the namespace chosen in the dialog", async () => {
    mount();
    planManifest.mockResolvedValue({
      docs: [
        {
          index: 0,
          apiVersion: "apps/v1",
          kind: "Deployment",
          name: "example",
          namespace: "kube-system",
          resource: "apps/v1/deployments",
          action: "create",
          unified: "",
          conflicts: [],
          warnings: [],
          error: null,
        },
      ],
    });

    const ns = document.querySelector('input[list="create-namespaces"]') as HTMLInputElement;
    fireEvent.change(ns, { target: { value: "kube-system" } });
    fireEvent.click(button("Preview"));
    await waitFor(() => expect(button("Create").disabled).toBe(false));

    fireEvent.click(button("Create"));
    await waitFor(() => expect(applied).not.toBeNull());
    expect(applied!.namespace).toBe("kube-system");
    expect(parse(applied!.yaml).kind).toBe("Deployment");
  });

  it("asks what to create when the page does not say", () => {
    mount({ resource: null });
    expect(screen.getByPlaceholderText("Filter kinds")).toBeTruthy();

    fireEvent.click(screen.getByText("Secret"));
    expect(screen.getByText(/Create Secret/)).toBeTruthy();
  });

  it("does not offer a kind this account cannot create", () => {
    mount({ resource: descriptor({ verbs: ["get", "list"] }) });
    // Falls through to the picker rather than opening a form that cannot apply.
    expect(screen.getByPlaceholderText("Filter kinds")).toBeTruthy();
    expect(screen.getByText(/cannot create Deployment here/)).toBeTruthy();
  });

  it("opens on YAML for a kind with no form layout", () => {
    mount({
      resource: descriptor({
        key: "example.com/v1/widgets",
        group: "example.com",
        kind: "Widget",
        plural: "widgets",
        apiVersion: "example.com/v1",
        isCrd: true,
      }),
    });
    expect(document.querySelector(".create__form")).toBeNull();
    expect(tab("Form").disabled).toBe(true);
  });
});
