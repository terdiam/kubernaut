// Monaco setup.
//
// Imports target `editor.api` plus the handful of contributions this app uses,
// not the `monaco-editor` barrel. The barrel pulls every bundled language
// (TypeScript, C#, Solidity, …) into the app bundle; we edit YAML only.
//
// Workers are bundled by Vite rather than loaded from a CDN, which is required
// anyway under the app's CSP (no external origins).
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/editor/contrib/bracketMatching/browser/bracketMatching.js";
import "monaco-editor/esm/vs/editor/contrib/comment/browser/comment.js";
import "monaco-editor/esm/vs/editor/contrib/contextmenu/browser/contextmenu.js";
import "monaco-editor/esm/vs/editor/contrib/find/browser/findController.js";
import "monaco-editor/esm/vs/editor/contrib/folding/browser/folding.js";
import "monaco-editor/esm/vs/editor/contrib/format/browser/formatActions.js";
import "monaco-editor/esm/vs/editor/contrib/hover/browser/hoverContribution.js";
import "monaco-editor/esm/vs/editor/contrib/indentation/browser/indentation.js";
import "monaco-editor/esm/vs/editor/contrib/linesOperations/browser/linesOperations.js";
import "monaco-editor/esm/vs/editor/contrib/multicursor/browser/multicursor.js";
import "monaco-editor/esm/vs/editor/contrib/suggest/browser/suggestController.js";
import "monaco-editor/esm/vs/editor/contrib/wordHighlighter/browser/wordHighlighter.js";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import { configureMonacoYaml } from "monaco-yaml";
import yamlWorker from "monaco-yaml/yaml.worker?worker";

declare global {
  interface Window {
    MonacoEnvironment?: monaco.Environment;
  }
}

window.MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    return label === "yaml" ? new yamlWorker() : new editorWorker();
  },
};

monaco.editor.defineTheme("kubernaut-dark", {
  base: "vs-dark",
  inherit: true,
  rules: [],
  colors: {
    "editor.background": "#0b1017",
    "editorGutter.background": "#0b1017",
    "editorLineNumber.foreground": "#3d4a5c",
    "editor.lineHighlightBackground": "#131c28",
    "editorIndentGuide.background1": "#1e2a3a",
  },
});

monaco.editor.defineTheme("kubernaut-light", {
  base: "vs",
  inherit: true,
  rules: [],
  colors: {
    "editor.background": "#ffffff",
    "editorGutter.background": "#ffffff",
    "editorLineNumber.foreground": "#98a6b8",
    "editor.lineHighlightBackground": "#f1f5fa",
    "editorIndentGuide.background1": "#dde4ee",
  },
});

/**
 * Editor theme for the current app theme.
 *
 * Read from the document rather than passed in, so the editor cannot drift out
 * of step with the rest of the window — including when "system" changes while
 * the app is open.
 */
export function editorTheme(): string {
  const setting = document.documentElement.dataset.theme ?? "system";
  const dark =
    setting === "dark" ||
    (setting !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  return dark ? "kubernaut-dark" : "kubernaut-light";
}

const yaml = configureMonacoYaml(monaco, {
  // Schemas come from the cluster's own OpenAPI, so no network fetching.
  enableSchemaRequest: false,
  validate: true,
  hover: true,
  completion: true,
  format: { singleQuote: false },
  schemas: [],
});

/**
 * Point a model's URI at a cluster-derived schema. Each resource type gets its
 * own synthetic URI so several editors can be open with different schemas.
 */
export function applySchema(resourceKey: string, schema: unknown) {
  const uri = `kubernaut://schema/${encodeURIComponent(resourceKey)}.json`;
  yaml.update({
    enableSchemaRequest: false,
    validate: true,
    hover: true,
    completion: true,
    format: { singleQuote: false },
    schemas: [
      {
        uri,
        fileMatch: [`*${encodeURIComponent(resourceKey)}*`],
        schema: schema as object,
      },
    ],
  });
}

export function modelUriFor(resourceKey: string, name: string) {
  return monaco.Uri.parse(
    `file:///${encodeURIComponent(resourceKey)}/${encodeURIComponent(name)}.yaml`,
  );
}

export { monaco };
