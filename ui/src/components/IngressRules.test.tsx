import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FormContext, invalidateLookups } from "../formContext";
import { IngressRulesField, portValue } from "./IngressRules";
import type { LookupOption } from "../types";

const lookupOptions =
  vi.fn<(c: string, source: string, ns: string | null, param: string | null) => Promise<LookupOption[]>>();

vi.mock("../api", () => ({
  api: {
    lookupOptions: (c: string, s: string, n: string | null, p: string | null) =>
      lookupOptions(c, s, n, p),
  },
}));

const SERVICES: LookupOption[] = [
  { value: "web", label: "web", detail: "ClusterIP · 80" },
  { value: "api", label: "api", detail: "ClusterIP · 8080" },
];
const WEB_PORTS: LookupOption[] = [{ value: "http", label: "80 · http", detail: "→ 8080" }];

function answer(source: string, param: string | null): Promise<LookupOption[]> {
  if (source === "services") return Promise.resolve(SERVICES);
  if (source === "servicePorts" && param === "web") return Promise.resolve(WEB_PORTS);
  return Promise.resolve([]);
}

function rules(onChange = vi.fn(), value: unknown = undefined) {
  render(
    <FormContext.Provider value={{ cluster: "default", namespace: "app", draft: {} }}>
      <IngressRulesField value={value} onChange={onChange} />
    </FormContext.Provider>,
  );
  return onChange;
}

/** [pathType, service, port] — the selects in one path row, in read order. */
const selects = () =>
  [...document.querySelectorAll(".rules__path select")] as HTMLSelectElement[];
const serviceSelect = () => selects()[1]!;
const portSelect = () => selects()[2]!;

const oneRule = () => [
  {
    host: "example.local",
    http: { paths: [{ path: "/", pathType: "Prefix", backend: { service: { name: "", port: {} } } }] },
  },
];

beforeEach(() => {
  invalidateLookups();
  lookupOptions.mockImplementation((_c, source, _ns, param) => answer(source, param));
});
afterEach(() => {
  cleanup();
  lookupOptions.mockReset();
});

describe("IngressRulesField", () => {
  it("offers services from the namespace, and no ports until one is chosen", async () => {
    rules(vi.fn(), oneRule());
    await screen.findByText("web");

    // Listing ports before a service is chosen would be listing nothing useful.
    expect(portSelect().disabled).toBe(true);
    expect(
      lookupOptions.mock.calls.some(([, source]) => source === "servicePorts"),
    ).toBe(false);
  });

  it("lists the chosen service's ports, and only those", async () => {
    const onChange = rules(vi.fn(), oneRule());
    await screen.findByText("web");

    fireEvent.change(serviceSelect(), { target: { value: "web" } });

    expect(onChange).toHaveBeenCalledWith([
      expect.objectContaining({
        http: { paths: [expect.objectContaining({ backend: { service: { name: "web", port: {} } } })] },
      }),
    ]);
  });

  it("clears the port when the service changes, because it belonged to the old one", async () => {
    const withPort = [
      {
        host: "example.local",
        http: {
          paths: [
            {
              path: "/",
              pathType: "Prefix",
              backend: { service: { name: "web", port: { number: 80 } } },
            },
          ],
        },
      },
    ];
    const onChange = rules(vi.fn(), withPort);
    await screen.findByText("api");

    fireEvent.change(serviceSelect(), { target: { value: "api" } });
    const written = onChange.mock.calls[0]![0] as Record<string, any>[];
    // Keeping port 80 against `api` would route to a port it does not expose.
    expect(written[0]!.http.paths[0].backend.service.port).toEqual({});
  });

  it("writes an unnamed port as a number", async () => {
    // A Service with no port name is offered by number, and must be written
    // as `port.number` — `port.name: "8080"` is rejected by the apiserver.
    lookupOptions.mockImplementation((_c, source, _ns, param) =>
      source === "servicePorts" && param === "web"
        ? Promise.resolve([{ value: "8080", label: "8080", detail: null }])
        : answer(source, param),
    );
    const onChange = rules(vi.fn(), [
      {
        host: "",
        http: {
          paths: [
            { path: "/", pathType: "Prefix", backend: { service: { name: "web", port: {} } } },
          ],
        },
      },
    ]);
    await waitFor(() => expect(portSelect().options.length).toBeGreaterThan(1));

    fireEvent.change(portSelect(), { target: { value: "8080" } });
    const written = onChange.mock.calls.at(-1)![0] as Record<string, any>[];
    expect(written[0]!.http.paths[0].backend.service.port).toEqual({ number: 8080 });
  });

  it("writes a named port as a name", async () => {
    const onChange = rules(vi.fn(), [
      {
        host: "",
        http: {
          paths: [
            { path: "/", pathType: "Prefix", backend: { service: { name: "web", port: {} } } },
          ],
        },
      },
    ]);
    await waitFor(() => expect(portSelect().options.length).toBeGreaterThan(1));

    fireEvent.change(portSelect(), { target: { value: "http" } });
    const written = onChange.mock.calls.at(-1)![0] as Record<string, any>[];
    // A name survives the Service being renumbered; a number does not.
    expect(written[0]!.http.paths[0].backend.service.port).toEqual({ name: "http" });
  });

  it("keeps a service or port the cluster no longer has, and flags it", async () => {
    rules(vi.fn(), [
      {
        host: "",
        http: {
          paths: [
            {
              path: "/",
              pathType: "Prefix",
              backend: { service: { name: "gone", port: { number: 9999 } } },
            },
          ],
        },
      },
    ]);
    // Silently blanking these would quietly rewrite an existing Ingress.
    expect(await screen.findByText("gone (not in this namespace)")).toBeTruthy();
    expect(screen.getByText("9999 (not on this service)")).toBeTruthy();
  });

  it("recognises a port stored by number against a service that names it", async () => {
    // The lookup offers a named port by its name (`value: "http"`) so a fresh
    // pick survives renumbering, but an existing manifest can already
    // reference the same port by number. Comparing raw values alone made that
    // legitimate port look like it had vanished from the service.
    rules(vi.fn(), [
      {
        host: "",
        http: {
          paths: [
            {
              path: "/",
              pathType: "Prefix",
              backend: { service: { name: "web", port: { number: 80 } } },
            },
          ],
        },
      },
    ]);
    await waitFor(() => expect(portSelect().options.length).toBeGreaterThan(1));

    expect(screen.queryByText("80 (not on this service)")).toBeNull();
    // The select must show the matching option as selected, not just avoid
    // the warning text — otherwise it renders with nothing chosen.
    expect(portSelect().value).toBe("http");
  });

  it("starts a host with one path already on it", () => {
    const onChange = rules();
    fireEvent.click(screen.getByText("Add host"));
    expect(onChange).toHaveBeenCalledWith([
      { host: "", http: { paths: [{ path: "/", pathType: "Prefix", backend: { service: { name: "", port: {} } } }] } },
    ]);
  });
});

describe("portValue", () => {
  it("reads both shapes Kubernetes allows", () => {
    expect(portValue({ number: 80 })).toBe("80");
    expect(portValue({ name: "http" })).toBe("http");
    expect(portValue({})).toBe("");
  });
});
