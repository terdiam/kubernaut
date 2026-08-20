import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DiagnosePane } from "./DiagnosePane";
import type { DiagnosisReport, StepAction } from "../types";

const diagnose = vi.fn<() => Promise<DiagnosisReport>>();
const openObject = vi.fn();

vi.mock("../api", () => ({
  api: { diagnose: () => diagnose() },
}));

vi.mock("../store", () => ({
  useStore: (select: (state: { openObject: typeof openObject }) => unknown) =>
    select({ openObject }),
}));

const crashLoop = (): DiagnosisReport => ({
  examined: 1,
  healthy: 0,
  truncated: false,
  pods: [
    {
      pod: "api-1",
      namespace: "app",
      phase: "Running",
      healthy: false,
      summary: "Container `api` keeps crashing",
      findings: [
        {
          severity: "error",
          code: "CrashLoopBackOff",
          title: "Container `api` keeps crashing",
          explanation: "The container starts, exits, and is restarted.",
          container: "api",
          evidence: ["api: CrashLoopBackOff after 7 restart(s)", "last exit code 137 (OOMKilled)"],
          steps: [
            {
              text: "Read the previous instance's logs.",
              command: "kubectl logs api-1 -n app -c api --previous",
              action: { kind: "logs", container: "api", previous: true } as StepAction,
            },
            {
              text: "Open the node.",
              command: null,
              action: {
                kind: "open",
                resource: "core/v1/nodes",
                namespace: null,
                name: "node-a",
              } as StepAction,
            },
          ],
        },
      ],
    },
  ],
});

function mount(onAction = vi.fn()) {
  render(
    <DiagnosePane
      cluster="default"
      resource="core/v1/pods"
      namespace="app"
      name="api-1"
      revision={0}
      onAction={onAction}
    />,
  );
  return onAction;
}

afterEach(() => {
  cleanup();
  diagnose.mockReset();
  openObject.mockReset();
});

describe("DiagnosePane", () => {
  it("quotes the cluster's own evidence verbatim", async () => {
    diagnose.mockResolvedValue(crashLoop());
    mount();

    // The exit code is the diagnosis; paraphrasing it would make the advice
    // uncheckable.
    expect(await screen.findByText("last exit code 137 (OOMKilled)")).toBeTruthy();
    expect(screen.getByText("CrashLoopBackOff")).toBeTruthy();
  });

  it("hands a logs step back to the drawer with its container and previous flag", async () => {
    diagnose.mockResolvedValue(crashLoop());
    const onAction = mount();

    fireEvent.click(await screen.findByText("Open previous logs"));
    expect(onAction).toHaveBeenCalledWith({ kind: "logs", container: "api", previous: true });
  });

  it("navigates for an open step rather than switching tabs", async () => {
    diagnose.mockResolvedValue(crashLoop());
    const onAction = mount();

    fireEvent.click(await screen.findByText("Open node-a"));
    expect(openObject).toHaveBeenCalledWith("core/v1/nodes", null, "node-a");
    expect(onAction).not.toHaveBeenCalled();
  });

  it("says so plainly when there is nothing to act on", async () => {
    diagnose.mockResolvedValue({ pods: [], examined: 3, healthy: 3, truncated: false });
    mount();

    await waitFor(() => expect(screen.getByText(/Nothing to act on/)).toBeTruthy());
    expect(screen.getByText(/3 pods examined/)).toBeTruthy();
  });
});
