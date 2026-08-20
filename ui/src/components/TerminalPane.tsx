import { useEffect, useRef, useState } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { api, openTerminal, type TerminalRequest } from "../api";
import type { ContainerInfo, TerminalDescriptor, TerminalEvent } from "../types";

type Mode = TerminalRequest["mode"];

interface Props {
  cluster: string;
  /** Pod exec and ephemeral debugging need these; a node shell does not. */
  namespace?: string;
  pod?: string;
  /** Set for a node terminal. */
  node?: string;
  /** Modes offered in the picker. */
  modes: Mode[];
}

const MODE_LABEL: Record<Mode, string> = {
  podExec: "Container shell",
  ephemeral: "Debug container",
  nodeShell: "Node shell",
  localShell: "Local kubectl shell",
};

const DEFAULT_IMAGE = "busybox:1.36";

export function TerminalPane({ cluster, namespace, pod, node, modes }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const [mode, setMode] = useState<Mode>(modes[0] ?? "podExec");
  const [containers, setContainers] = useState<ContainerInfo[]>([]);
  const [container, setContainer] = useState<string | null>(null);
  const [image, setImage] = useState(DEFAULT_IMAGE);
  const [descriptor, setDescriptor] = useState<TerminalDescriptor | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [closed, setClosed] = useState<string | null>(null);
  /** Bumped to (re)connect; 0 means nothing has been started yet. */
  const [attempt, setAttempt] = useState(0);
  const [pendingConfirm, setPendingConfirm] = useState(false);

  useEffect(() => {
    if (!pod || !namespace) return;
    let cancelled = false;
    api
      .podContainers(cluster, namespace, pod)
      .then((list) => {
        if (cancelled) return;
        setContainers(list);
        const app = list.find((c) => c.role === "app");
        setContainer((current) => current ?? app?.name ?? null);
      })
      .catch((err) => !cancelled && setError(String(err)));
    return () => {
      cancelled = true;
    };
  }, [cluster, namespace, pod]);

  // Modes that create or mutate cluster objects ask before doing it.
  const needsConfirmation = mode === "ephemeral" || mode === "nodeShell";
  const confirmationTarget = mode === "nodeShell" ? (node ?? "") : (pod ?? "");

  const buildRequest = (): TerminalRequest | null => {
    switch (mode) {
      case "podExec":
        if (!namespace || !pod) return null;
        return { mode, namespace, pod, container, command: [] };
      case "ephemeral":
        if (!namespace || !pod) return null;
        return {
          mode,
          namespace,
          pod,
          targetContainer: container,
          image,
          confirmation: pod,
        };
      case "nodeShell":
        if (!node) return null;
        return { mode, node, namespace: namespace ?? "default", image, confirmation: node };
      case "localShell":
        return { mode, namespace: namespace ?? null };
    }
  };

  useEffect(() => {
    if (attempt === 0 || !host.current) return;

    const term = new Terminal({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      fontSize: 12,
      cursorBlink: true,
      convertEol: true,
      // Read from the stylesheet so the terminal matches whichever theme is
      // active, rather than pinning the dark palette.
      theme: (() => {
        const style = getComputedStyle(document.documentElement);
        const token = (name: string, fallback: string) =>
          style.getPropertyValue(name).trim() || fallback;
        return {
          background: token("--bg", "#0b1017"),
          foreground: token("--text", "#dbe4f0"),
          cursor: token("--accent", "#38bdf8"),
        };
      })(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(host.current);
    fit.fit();

    let session: { descriptor: TerminalDescriptor; close: () => void } | null = null;
    let disposed = false;
    setClosed(null);
    setError(null);
    setStatus(null);

    const onData = term.onData((data) => {
      if (session) void api.terminalWrite(session.descriptor.sessionId, data);
    });

    const resize = () => {
      fit.fit();
      if (session) {
        void api.terminalResize(session.descriptor.sessionId, term.cols, term.rows);
      }
    };
    const observer = new ResizeObserver(resize);
    observer.observe(host.current);

    const handle = (events: TerminalEvent[]) => {
      for (const event of events) {
        switch (event.type) {
          case "output":
            term.write(event.data);
            break;
          case "status":
            setStatus(event.message);
            term.write(`\x1b[2m${event.message}\x1b[0m\r\n`);
            break;
          case "closed":
            setClosed(event.status);
            term.write(`\r\n\x1b[2m— ${event.status} —\x1b[0m\r\n`);
            break;
          case "failed":
            setError(event.message);
            term.write(`\r\n\x1b[31m${event.message}\x1b[0m\r\n`);
            break;
        }
      }
    };

    const request = buildRequest();
    if (!request) {
      setError("This terminal mode needs a target that is not available here.");
      return () => {
        observer.disconnect();
        onData.dispose();
        term.dispose();
      };
    }

    openTerminal(cluster, request, { columns: term.cols, rows: term.rows }, handle)
      .then((opened) => {
        if (disposed) {
          opened.close();
          return;
        }
        session = opened;
        setDescriptor(opened.descriptor);
        term.focus();
      })
      .catch((err) => !disposed && setError(String(err)));

    return () => {
      disposed = true;
      observer.disconnect();
      onData.dispose();
      session?.close();
      term.dispose();
    };
    // `mode`, `container` and `image` are read through buildRequest at start.
  }, [cluster, attempt]);

  const start = () => {
    if (needsConfirmation && attempt === 0) {
      setPendingConfirm(true);
      return;
    }
    setDescriptor(null);
    setAttempt((n) => n + 1);
  };

  return (
    <div className="terminal">
      <div className="terminal__toolbar">
        {modes.length > 1 && (
          <select
            value={mode}
            disabled={attempt > 0 && !closed}
            onChange={(e) => {
              setMode(e.target.value as Mode);
              setAttempt(0);
              setDescriptor(null);
            }}
          >
            {modes.map((entry) => (
              <option key={entry} value={entry}>
                {MODE_LABEL[entry]}
              </option>
            ))}
          </select>
        )}

        {(mode === "podExec" || mode === "ephemeral") && containers.length > 1 && (
          <select
            value={container ?? ""}
            disabled={attempt > 0 && !closed}
            onChange={(e) => setContainer(e.target.value || null)}
          >
            {containers.map((c) => (
              <option key={c.name} value={c.name}>
                {c.name}
                {c.role !== "app" ? ` (${c.role})` : ""}
              </option>
            ))}
          </select>
        )}

        {(mode === "ephemeral" || mode === "nodeShell") && (
          <input
            className="terminal__image"
            value={image}
            disabled={attempt > 0 && !closed}
            onChange={(e) => setImage(e.target.value)}
            title="Debug image"
          />
        )}

        {(attempt === 0 || closed) && (
          <button className="button button--primary" onClick={start}>
            {attempt === 0 ? "Start" : "Reconnect"}
          </button>
        )}

        <span className="muted">{descriptor?.title ?? pod ?? node ?? cluster}</span>
        {status && !closed && <span className="muted">{status}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      {descriptor?.warning && <p className="warning terminal__warning">{descriptor.warning}</p>}

      {attempt === 0 && (
        <p className="muted terminal__idle">
          {MODE_LABEL[mode]}
          {needsConfirmation
            ? " changes the cluster — press Start to review what it will do."
            : " — press Start to connect."}
        </p>
      )}

      <div className="terminal__host" ref={host} />

      {pendingConfirm && (
        <ConfirmStart
          mode={mode}
          target={confirmationTarget}
          image={image}
          onCancel={() => setPendingConfirm(false)}
          onConfirm={() => {
            setPendingConfirm(false);
            setDescriptor(null);
            setAttempt((n) => n + 1);
          }}
        />
      )}
    </div>
  );
}

function ConfirmStart({
  mode,
  target,
  image,
  onCancel,
  onConfirm,
}: {
  mode: Mode;
  target: string;
  image: string;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const [typed, setTyped] = useState("");

  const explanation =
    mode === "nodeShell" ? (
      <>
        <p>
          This creates a <strong>privileged pod</strong> on <code>{target}</code> with host PID,
          network and IPC, running <code>nsenter</code> into the node's own namespaces. That is
          effectively root on the machine.
        </p>
        <p className="muted">
          The pod uses <code>{image}</code> and is deleted when this terminal closes.
        </p>
      </>
    ) : (
      <>
        <p>
          This adds an <strong>ephemeral debug container</strong> to <code>{target}</code> using{" "}
          <code>{image}</code>.
        </p>
        <p className="warning">
          Ephemeral containers cannot be removed or changed. It stays on the pod until the pod
          itself is recreated.
        </p>
      </>
    );

  return (
    <div className="modal" role="dialog">
      <div className="modal__card">
        <h3>{mode === "nodeShell" ? "Open a node shell" : "Attach a debug container"}</h3>
        {explanation}
        <div className="field">
          <label className="field__label">
            Type <code>{target}</code> to confirm
          </label>
          <input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
        </div>
        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="button button--danger"
            disabled={typed !== target}
            onClick={onConfirm}
          >
            {mode === "nodeShell" ? "Create pod" : "Attach"}
          </button>
        </div>
      </div>
    </div>
  );
}
