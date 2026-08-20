import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FormContext, invalidateLookups } from "../formContext";
import { LookupField, RefListField, VolumesField } from "./LookupFields";
import type { LookupOption } from "../types";

const lookupOptions = vi.fn<
  (cluster: string, source: string, namespace: string | null, param: string | null) =>
    Promise<LookupOption[]>
>();

vi.mock("../api", () => ({
  api: {
    lookupOptions: (c: string, s: string, n: string | null, p: string | null) =>
      lookupOptions(c, s, n, p),
  },
}));

const option = (value: string, detail: string | null = null): LookupOption => ({
  value,
  label: value,
  detail,
});

function scope(children: React.ReactNode, draft: unknown = {}) {
  return render(
    <FormContext.Provider value={{ cluster: "default", namespace: "app", draft }}>
      {children}
    </FormContext.Provider>,
  );
}

beforeEach(() => invalidateLookups());
afterEach(() => {
  cleanup();
  lookupOptions.mockReset();
});

describe("LookupField", () => {
  it("offers what the cluster has, scoped to the namespace", async () => {
    lookupOptions.mockResolvedValue([option("build-bot"), option("deploy-bot")]);
    const onChange = vi.fn();
    scope(<LookupField id="sa" source="serviceAccounts" value="" onChange={onChange} />);

    await screen.findByText("build-bot");
    expect(lookupOptions).toHaveBeenCalledWith("default", "serviceAccounts", "app", null);

    fireEvent.change(screen.getByRole("combobox"), { target: { value: "deploy-bot" } });
    expect(onChange).toHaveBeenCalledWith("deploy-bot");
  });

  it("clears the field rather than writing an empty string", async () => {
    lookupOptions.mockResolvedValue([option("build-bot")]);
    const onChange = vi.fn();
    scope(<LookupField id="sa" source="serviceAccounts" value="build-bot" onChange={onChange} />);

    await screen.findByText("build-bot");
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "" } });
    // `serviceAccountName: ""` is not the same as absent, and applies badly.
    expect(onChange).toHaveBeenCalledWith(undefined);
  });

  it("narrows by the field it depends on, and refetches when that changes", async () => {
    lookupOptions.mockResolvedValue([option("web")]);
    const { rerender } = scope(
      <LookupField
        id="target"
        source="workloads"
        dependsOn="spec.scaleTargetRef.kind"
        value=""
        onChange={vi.fn()}
      />,
      { spec: { scaleTargetRef: { kind: "Deployment" } } },
    );
    await waitFor(() =>
      expect(lookupOptions).toHaveBeenCalledWith("default", "workloads", "app", "Deployment"),
    );

    rerender(
      <FormContext.Provider
        value={{
          cluster: "default",
          namespace: "app",
          draft: { spec: { scaleTargetRef: { kind: "StatefulSet" } } },
        }}
      >
        <LookupField
          id="target"
          source="workloads"
          dependsOn="spec.scaleTargetRef.kind"
          value=""
          onChange={vi.fn()}
        />
      </FormContext.Provider>,
    );
    await waitFor(() =>
      expect(lookupOptions).toHaveBeenCalledWith("default", "workloads", "app", "StatefulSet"),
    );
  });

  it("falls back to typing when the cluster cannot be asked", async () => {
    lookupOptions.mockRejectedValue(new Error("forbidden"));
    scope(<LookupField id="sc" source="storageClasses" allowCustom value="" onChange={vi.fn()} />);

    // A lookup that fails must not make the field unusable.
    await screen.findByText(/could not list/);
    expect(screen.getByRole("textbox")).toBeTruthy();
  });

  it("keeps a value the list does not contain", async () => {
    lookupOptions.mockResolvedValue([option("nginx")]);
    scope(
      <LookupField id="ic" source="ingressClasses" allowCustom value="traefik" onChange={vi.fn()} />,
    );
    // Blanking a field because the object is not created yet would lose work.
    await waitFor(() => expect(screen.getByDisplayValue("traefik")).toBeTruthy());
  });
});

describe("RefListField", () => {
  it("writes the { name } shape imagePullSecrets uses", async () => {
    lookupOptions.mockResolvedValue([option("regcred", "kubernetes.io/dockerconfigjson")]);
    const onChange = vi.fn();
    scope(<RefListField source="dockerConfigSecrets" value={undefined} onChange={onChange} />);

    await waitFor(() => expect(lookupOptions).toHaveBeenCalled());
    fireEvent.click(screen.getByText("Add"));
    expect(onChange).toHaveBeenCalledWith([{ name: "regcred" }]);
  });

  it("says why the list is empty instead of showing a blank select", async () => {
    lookupOptions.mockResolvedValue([]);
    scope(<RefListField source="dockerConfigSecrets" value={undefined} onChange={vi.fn()} />);
    // Namespacing is the usual cause and is not obvious.
    expect(await screen.findByText(/pull secret is namespaced/)).toBeTruthy();
  });

  it("drops an entry to undefined rather than leaving an empty array", async () => {
    lookupOptions.mockResolvedValue([option("regcred")]);
    const onChange = vi.fn();
    scope(<RefListField source="dockerConfigSecrets" value={[{ name: "regcred" }]} onChange={onChange} />);

    await waitFor(() => expect(lookupOptions).toHaveBeenCalled());
    fireEvent.click(screen.getByLabelText("Remove"));
    expect(onChange).toHaveBeenCalledWith(undefined);
  });
});

describe("VolumesField", () => {
  it("edits claim-backed volumes and leaves the rest alone", async () => {
    lookupOptions.mockResolvedValue([option("data-0", "Bound 10Gi")]);
    const onChange = vi.fn();
    const volumes = [
      { name: "config", configMap: { name: "settings" } },
      { name: "data", persistentVolumeClaim: { claimName: "data-0" } },
    ];
    scope(<VolumesField value={volumes} onChange={onChange} />);

    // A volume the form does not model must survive an edit to one it does.
    expect(await screen.findByText(/kept as they are/)).toBeTruthy();

    fireEvent.change(screen.getByDisplayValue("data"), { target: { value: "storage" } });
    expect(onChange).toHaveBeenCalledWith([
      { name: "config", configMap: { name: "settings" } },
      { name: "storage", persistentVolumeClaim: { claimName: "data-0" } },
    ]);
  });
});
