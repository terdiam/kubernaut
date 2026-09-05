import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { UpdateBanner } from "./UpdateBanner";

const check = vi.fn();

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: () => check(),
}));
vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: vi.fn(),
}));

let preferences: { checkUpdatesOnStartup: boolean } | null = null;
vi.mock("../store", () => ({
  useStore: (select: (s: { preferences: typeof preferences }) => unknown) =>
    select({ preferences }),
}));

afterEach(() => {
  cleanup();
  check.mockReset();
  preferences = null;
});

describe("UpdateBanner", () => {
  it("says nothing while checking is off", () => {
    preferences = { checkUpdatesOnStartup: false };
    render(<UpdateBanner />);
    expect(check).not.toHaveBeenCalled();
    expect(document.body.textContent).toBe("");
  });

  it("surfaces a failed check instead of going silent", async () => {
    // The exact bug this guards against: the check moves back to an idle
    // stage on failure, and an early return keyed only on that stage used to
    // hide the error along with it — a broken update check looked identical
    // to "no update available".
    preferences = { checkUpdatesOnStartup: true };
    check.mockRejectedValue(new Error("could not verify update signature"));
    render(<UpdateBanner />);

    expect(
      await screen.findByText(/Could not check for updates: Error: could not verify/),
    ).toBeTruthy();
  });

  it("shows the update prompt when one is found", async () => {
    preferences = { checkUpdatesOnStartup: true };
    check.mockResolvedValue({ version: "0.1.8" });
    render(<UpdateBanner />);

    await waitFor(() => expect(screen.getByText(/Version 0.1.8 is available/)).toBeTruthy());
    expect(screen.getByText("Download and install")).toBeTruthy();
  });

  it("says nothing when the check finds no update", async () => {
    preferences = { checkUpdatesOnStartup: true };
    check.mockResolvedValue(null);
    render(<UpdateBanner />);

    await waitFor(() => expect(check).toHaveBeenCalled());
    expect(document.body.textContent).toBe("");
  });
});
