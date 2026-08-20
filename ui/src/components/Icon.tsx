/**
 * Icon set.
 *
 * Inline SVG on a 16-unit grid rather than emoji or box-drawing characters:
 * those render at a different weight and baseline on every platform, which is
 * why the sidebar looked ragged. Every icon inherits `currentColor`, so themes
 * need no icon variants.
 */

import type { ReactNode } from "react";

export type IconName =
  | "overview"
  | "node"
  | "namespace"
  | "events"
  | "workloads"
  | "config"
  | "network"
  | "storage"
  | "access"
  | "custom"
  | "other"
  | "helm"
  | "gitops"
  | "security"
  | "settings"
  | "cluster"
  | "plus";

const PATHS: Record<IconName, ReactNode> = {
  overview: (
    <>
      <rect x="2" y="2" width="5.5" height="5.5" rx="1" />
      <rect x="8.5" y="2" width="5.5" height="9" rx="1" />
      <rect x="2" y="8.5" width="5.5" height="5.5" rx="1" />
      <rect x="8.5" y="12" width="5.5" height="2" rx="1" />
    </>
  ),
  node: (
    <>
      <rect x="2" y="3" width="12" height="4" rx="1" />
      <rect x="2" y="9" width="12" height="4" rx="1" />
      <path d="M4.5 5h.01M4.5 11h.01" />
    </>
  ),
  namespace: (
    <>
      <path d="M8 2 14 5 8 8 2 5z" />
      <path d="M2 8l6 3 6-3" />
      <path d="M2 11l6 3 6-3" />
    </>
  ),
  events: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 4.5V8l2.5 1.5" />
    </>
  ),
  workloads: (
    <>
      <path d="M8 1.8 14 5v6l-6 3.2L2 11V5z" />
      <path d="M2 5l6 3.2L14 5" />
      <path d="M8 8.2v6" />
    </>
  ),
  config: (
    <>
      <path d="M2 4h6M11 4h3M2 12h3M8 12h6" />
      <circle cx="9.5" cy="4" r="1.6" />
      <circle cx="6.5" cy="12" r="1.6" />
      <path d="M2 8h12" />
    </>
  ),
  network: (
    <>
      <path d="M5 13V5m0 0L2.6 7.4M5 3.2 7.4 5.6" />
      <path d="M11 3v8m0 0 2.4-2.4M11 12.8 8.6 10.4" />
    </>
  ),
  storage: (
    <>
      <ellipse cx="8" cy="4" rx="5.5" ry="2.2" />
      <path d="M2.5 4v8c0 1.2 2.5 2.2 5.5 2.2s5.5-1 5.5-2.2V4" />
      <path d="M2.5 8c0 1.2 2.5 2.2 5.5 2.2s5.5-1 5.5-2.2" />
    </>
  ),
  access: (
    <>
      <path d="M8 1.8 13.2 4v4.2c0 3.1-2.2 5.3-5.2 6-3-.7-5.2-2.9-5.2-6V4z" />
      <circle cx="8" cy="7" r="1.6" />
      <path d="M8 8.6v2.2" />
    </>
  ),
  custom: (
    <>
      <path d="M8 1.6l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 10.4 4.5 12.3l.8-3.9-2.9-2.7 3.9-.5z" />
    </>
  ),
  other: (
    <>
      <circle cx="4" cy="8" r="1.1" />
      <circle cx="8" cy="8" r="1.1" />
      <circle cx="12" cy="8" r="1.1" />
    </>
  ),
  helm: (
    <>
      <circle cx="8" cy="8" r="5.4" />
      <circle cx="8" cy="8" r="1.8" />
      <path d="M8 2.6v3.6M8 9.8v3.6M2.6 8h3.6M9.8 8h3.6" />
    </>
  ),
  gitops: (
    <>
      <circle cx="4.5" cy="3.8" r="1.8" />
      <circle cx="4.5" cy="12.2" r="1.8" />
      <circle cx="11.5" cy="8" r="1.8" />
      <path d="M4.5 5.6v4.8" />
      <path d="M6.3 3.8h2.4a1.4 1.4 0 0 1 1.4 1.4v1.2" />
    </>
  ),
  security: (
    <>
      <path d="M8 1.8 13.2 4v4.2c0 3.1-2.2 5.3-5.2 6-3-.7-5.2-2.9-5.2-6V4z" />
      <path d="M5.8 8.1 7.3 9.6l3-3.2" />
    </>
  ),
  settings: (
    <>
      <circle cx="8" cy="8" r="2.1" />
      <path d="M8 1.8v1.8M8 12.4v1.8M14.2 8h-1.8M3.6 8H1.8M12.4 3.6l-1.3 1.3M4.9 11.1l-1.3 1.3M12.4 12.4l-1.3-1.3M4.9 4.9 3.6 3.6" />
    </>
  ),
  cluster: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M2 8h12" />
      <path d="M8 2c1.9 2 2.9 4 2.9 6S9.9 12 8 14C6.1 12 5.1 10 5.1 8S6.1 4 8 2z" />
    </>
  ),
  plus: (
    <>
      <path d="M8 3.2v9.6M3.2 8h9.6" />
    </>
  ),
};

export function Icon({ name, className }: { name: IconName; className?: string }) {
  return (
    <svg
      className={className ? `icon ${className}` : "icon"}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.3}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  );
}
