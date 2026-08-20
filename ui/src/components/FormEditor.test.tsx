import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FormEditor } from "./FormEditor";
import type { ApplyOutcome, EditRequest } from "../types";

const applyEdit = vi.fn<(cluster: string, request: EditRequest) => Promise<ApplyOutcome>>();

vi.mock("../api", () => ({
  api: {
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
});
