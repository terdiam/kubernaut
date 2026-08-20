import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { IssuePanel } from "./IssuePanel";
import { useStore } from "../store";
import type { Issue } from "../types";

vi.mock("../api", () => ({
  api: {},
  startWatch: vi.fn(),
}));

const issue = (over: Partial<Issue> = {}): Issue => ({
  severity: "error",
  kind: "Pod",
  resource: "core/v1/pods",
  namespace: "production",
  name: "cleanup-orphaned-artifacts-29283840-abcde",
  message: "Pod failed",
  ...over,
});

describe("IssuePanel", () => {
  let openObject: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    openObject = vi.fn(async () => {});
    useStore.setState({ openObject });
  });

  afterEach(cleanup);

  it("opens the object the issue is about", () => {
    render(<IssuePanel issues={[issue()]} />);
    fireEvent.click(screen.getByRole("button"));
    expect(openObject).toHaveBeenCalledWith(
      "core/v1/pods",
      "production",
      "cleanup-orphaned-artifacts-29283840-abcde",
    );
  });

  it("leaves no row disabled", () => {
    // The original bug: rows were enabled only when the object happened to be
    // in the table already open, and on the overview none ever is.
    render(
      <IssuePanel
        issues={[issue(), issue({ kind: "Node", resource: "core/v1/nodes", namespace: null })]}
      />,
    );
    for (const button of screen.getAllByRole("button")) {
      expect((button as HTMLButtonElement).disabled).toBe(false);
    }
  });

  it("opens a cluster-scoped object with no namespace", () => {
    render(<IssuePanel issues={[issue({ kind: "Node", resource: "core/v1/nodes", namespace: null, name: "node1" })]} />);
    fireEvent.click(screen.getByRole("button"));
    expect(openObject).toHaveBeenCalledWith("core/v1/nodes", null, "node1");
  });

  it("keeps generated names distinguishable", () => {
    render(
      <IssuePanel
        issues={[
          issue({ name: "cleanup-orphaned-artifacts-29283840-abcde" }),
          issue({ name: "cleanup-orphaned-artifacts-29283840-zyxwv" }),
        ]}
      />,
    );
    const [first, second] = screen.getAllByRole("button").map((b) => b.textContent);
    expect(first).toContain("…");
    expect(first).not.toBe(second);
  });
});
