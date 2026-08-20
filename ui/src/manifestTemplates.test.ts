import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { TEMPLATES, templateGroups } from "./manifestTemplates";

/** Same split the backend does: documents separated by `---`. */
function documents(yaml: string) {
  return parseAllDocuments(yaml).map((doc) => doc.toJS() as Record<string, unknown>);
}

describe("manifest templates", () => {
  it.each(TEMPLATES.map((t) => [t.id, t] as const))(
    "%s parses and is applyable as written",
    (_id, template) => {
      const docs = documents(template.yaml);
      expect(docs.length).toBeGreaterThan(0);

      for (const doc of docs) {
        // The backend refuses a document missing any of these, so a template
        // that lacks one would fail the moment someone picked it.
        expect(doc.apiVersion, `${template.id}: apiVersion`).toBeTruthy();
        expect(doc.kind, `${template.id}: kind`).toBeTruthy();
        const metadata = doc.metadata as Record<string, unknown> | undefined;
        expect(metadata?.name, `${template.id}: metadata.name`).toBeTruthy();

        // generateName cannot be server-side applied; a template must not use it.
        expect(metadata?.generateName).toBeUndefined();
        // Server-owned fields would make the create fail.
        expect(metadata?.resourceVersion).toBeUndefined();
        expect(metadata?.uid).toBeUndefined();
        expect(doc.status).toBeUndefined();
      }
    },
  );

  it("keeps the web bundle's three objects wired to each other", () => {
    const web = TEMPLATES.find((t) => t.id === "web");
    const docs = documents(web!.yaml);
    expect(docs.map((d) => d.kind)).toEqual(["Deployment", "Service", "Ingress"]);

    // A bundle whose Service selects nothing, or whose Ingress routes to a
    // Service that is not there, is worse than no template at all.
    const deployment = docs[0] as any;
    const service = docs[1] as any;
    const ingress = docs[2] as any;

    expect(service.spec.selector).toEqual(deployment.spec.selector.matchLabels);
    expect(service.spec.ports[0].targetPort).toBe(
      deployment.spec.template.spec.containers[0].ports[0].containerPort,
    );

    const backend = ingress.spec.rules[0].http.paths[0].backend.service;
    expect(backend.name).toBe(service.metadata.name);
    expect(backend.port.number).toBe(service.spec.ports[0].port);
  });

  it("has unique ids and groups every template", () => {
    const ids = TEMPLATES.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(templateGroups().flatMap((g) => g.templates)).toHaveLength(TEMPLATES.length);
  });
});
