import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Wizard } from "./Wizard";
import type { Section } from "../formSpec";

const SECTIONS: Section[] = [
  { title: "Scale", fields: [{ kind: "number", path: "spec.replicas", label: "Replicas" }] },
  { title: "Image", fields: [{ kind: "text", path: "spec.image", label: "Image" }] },
  { title: "Extra", fields: [{ kind: "text", path: "spec.note", label: "Note" }] },
];

function mount(draft: Record<string, unknown> = {}, onChange = vi.fn()) {
  render(<Wizard sections={SECTIONS} draft={draft} onChange={onChange} />);
  return onChange;
}

afterEach(cleanup);

describe("Wizard", () => {
  it("shows only the current step's fields, not the whole layout at once", () => {
    mount();
    expect(screen.getByLabelText("Replicas")).toBeTruthy();
    expect(screen.queryByLabelText("Image")).toBeNull();
    expect(screen.queryByLabelText("Note")).toBeNull();
  });

  it("advances with Next and keeps the draft across the step change", () => {
    const onChange = mount();
    fireEvent.change(screen.getByLabelText("Replicas"), { target: { value: "3" } });
    expect(onChange).toHaveBeenCalledWith("spec.replicas", 3);

    fireEvent.click(screen.getByText("Next →"));
    expect(screen.getByLabelText("Image")).toBeTruthy();
    expect(screen.queryByLabelText("Replicas")).toBeNull();
  });

  it("goes back", () => {
    mount();
    fireEvent.click(screen.getByText("Next →"));
    expect(screen.getByLabelText("Image")).toBeTruthy();

    fireEvent.click(screen.getByText("← Back"));
    expect(screen.getByLabelText("Replicas")).toBeTruthy();
  });

  it("disables Back on the first step and Next on the last", () => {
    mount();
    expect(screen.getByText("← Back").closest("button")!.disabled).toBe(true);
    expect(screen.getByText("Next →").closest("button")!.disabled).toBe(false);

    fireEvent.click(screen.getByText("Next →"));
    fireEvent.click(screen.getByText("Next →"));
    expect(screen.getByText("Next →").closest("button")!.disabled).toBe(true);
  });

  it("jumps to any step from the rail, so a mistake earlier does not force replaying every step", () => {
    mount();
    fireEvent.click(screen.getByText("Extra"));
    expect(screen.getByLabelText("Note")).toBeTruthy();

    fireEvent.click(screen.getByText("Scale"));
    expect(screen.getByLabelText("Replicas")).toBeTruthy();
  });

  it("marks a step done once you have moved past it", () => {
    mount();
    fireEvent.click(screen.getByText("Next →"));
    const scaleTab = screen.getByText("Scale").closest("button")!;
    expect(scaleTab.className).toContain("wizard__step--done");
  });
});
