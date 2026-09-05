import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { availableZones, effectiveZone, formatDateTime, zoneLabel } from "../time";
import type {
  CrashReport,
  Diagnostics,
  ManagedKubeconfig,
  Preferences,
  Theme,
} from "../types";

const THEMES: { id: Theme; label: string; help: string }[] = [
  { id: "system", label: "System", help: "Follow the operating system" },
  { id: "light", label: "Light", help: "" },
  { id: "dark", label: "Dark", help: "" },
];

export function Settings() {
  const contexts = useStore((s) => s.contexts);
  const preferences = useStore((s) => s.preferences);
  const savePreferences = useStore((s) => s.savePreferences);

  const [draft, setDraft] = useState<Preferences | null>(preferences);
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [crash, setCrash] = useState<CrashReport | null>(null);
  const [managed, setManaged] = useState<ManagedKubeconfig[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => setDraft(preferences), [preferences]);
  useEffect(() => {
    void api.diagnostics().then(setDiagnostics).catch(() => {});
    void api.lastCrash().then(setCrash).catch(() => {});
    void api.managedKubeconfigs().then(setManaged).catch(() => {});
  }, []);

  if (!draft) return <p className="muted overview__note">Loading preferences…</p>;

  const patch = (change: Partial<Preferences>) => setDraft({ ...draft, ...change });

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await savePreferences(draft);
      setStatus("Saved.");
      setDiagnostics(await api.diagnostics());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const dirty = JSON.stringify(draft) !== JSON.stringify(preferences);

  return (
    <div className="settings">
      <div className="settings__body">
        <section className="context__block">
          <h3>Appearance</h3>
          <div className="field">
            <label className="field__label">Theme</label>
            <div className="segmented">
              {THEMES.map((theme) => (
                <button
                  key={theme.id}
                  className={`segmented__button${
                    draft.theme === theme.id ? " segmented__button--active" : ""
                  }`}
                  title={theme.help}
                  onClick={() => patch({ theme: theme.id })}
                >
                  {theme.label}
                </button>
              ))}
            </div>
          </div>
        </section>

        <section className="context__block">
          <h3>Imported clusters</h3>
          <p className="muted settings__help">
            Kubeconfigs added through the <strong>+</strong> button on the cluster rail. They live
            in this app's configuration; your <code>~/.kube/config</code> is never modified, so
            these clusters are visible here and not to kubectl. Removing one forgets the file —
            the cluster itself is untouched.
          </p>
          {managed.length === 0 ? (
            <p className="muted">None imported.</p>
          ) : (
            <ul className="related">
              {managed.map((entry) => (
                <li key={entry.file}>
                  <div className="related__item" title={entry.file}>
                    <span className="related__kind">{entry.label}</span>
                    <span className="related__name">{entry.contexts.join(", ")}</span>
                    <button
                      className="icon-button"
                      title="Forget this kubeconfig"
                      onClick={() =>
                        void api
                          .removeKubeconfig(entry.file)
                          .then((contexts) => {
                            useStore.getState().setContexts(contexts);
                            return api.managedKubeconfigs();
                          })
                          .then(setManaged)
                          .catch((err) => setError(String(err)))
                      }
                    >
                      ✕
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="context__block">
          <h3>Protected contexts</h3>
          <p className="muted settings__help">
            Destructive actions against these contexts are refused outright — no dialog, no
            override. A confirmation stops accidents; it does not stop habit, and after the tenth
            one people type the name without reading it.
          </p>
          <div className="kv">
            {contexts.map((context) => {
              const on = draft.protectedContexts.includes(context.name);
              return (
                <label key={context.name} className="checkbox settings__context">
                  <input
                    type="checkbox"
                    checked={on}
                    onChange={(e) =>
                      patch({
                        protectedContexts: e.target.checked
                          ? [...draft.protectedContexts, context.name]
                          : draft.protectedContexts.filter((name) => name !== context.name),
                      })
                    }
                  />
                  {context.name}
                  {context.server && <span className="muted"> · {context.server}</span>}
                </label>
              );
            })}
            {contexts.length === 0 && <p className="muted">No contexts in the kubeconfig.</p>}
          </div>
        </section>

        <section className="context__block">
          <h3>Authentication plugin PATH</h3>
          <p className="muted settings__help">
            Contexts that authenticate through <code>aws</code>, <code>gcloud</code>,{" "}
            <code>az</code> or <code>kubelogin</code> need those binaries on <code>PATH</code>.
            The app recovers the login shell's PATH at startup; add directories here when that is
            not enough — a custom launcher, a nix profile, or a shell whose startup times out.
          </p>
          <StringList
            values={draft.extraPathEntries}
            placeholder="/opt/homebrew/bin"
            onChange={(extraPathEntries) => patch({ extraPathEntries })}
          />
          {diagnostics && (
            <details className="settings__details">
              <summary>Current PATH ({diagnostics.pathEntries.length} entries)</summary>
              <ul className="settings__paths">
                {diagnostics.pathEntries.map((entry) => (
                  <li key={entry}>{entry}</li>
                ))}
              </ul>
            </details>
          )}
        </section>

        <section className="context__block">
          <h3>Time zone</h3>
          <p className="muted settings__help">
            Kubernetes records every timestamp in UTC. Absolute times shown in the app are
            converted to this zone; the YAML tab is left alone, because it shows what the cluster
            actually stores.
          </p>
          <div className="field">
            <label className="field__label">Display timestamps in</label>
            <input
              list="kubernaut-timezones"
              value={draft.timezone}
              onChange={(e) => patch({ timezone: e.target.value || "system" })}
            />
            <datalist id="kubernaut-timezones">
              <option value="system" />
              <option value="UTC" />
              {availableZones().map((zone) => (
                <option key={zone} value={zone} />
              ))}
            </datalist>
          </div>
          <p className="muted settings__help">
            Currently <strong>{effectiveZone(draft.timezone)}</strong> ({zoneLabel(draft.timezone)}
            ). Right now that is{" "}
            <strong>{formatDateTime(new Date().toISOString(), draft.timezone)}</strong>.
          </p>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={draft.showAbsoluteTimes}
              onChange={(e) => patch({ showAbsoluteTimes: e.target.checked })}
            />
            Show absolute timestamps beside ages
          </label>
          <p className="muted settings__help">
            Ages like <code>3h12m</code> answer "how long ago"; an absolute time answers "was it
            before the deploy". Turn this on to get both in tables.
          </p>
        </section>

        <section className="context__block">
          <h3>Logs</h3>
          <div className="field">
            <label className="field__label">
              Lines of history when a log view opens
              <span className="field__help">
                Larger values take longer to load on a chatty workload.
              </span>
            </label>
            <input
              type="number"
              min={10}
              max={20000}
              value={draft.logTailLines}
              onChange={(e) => patch({ logTailLines: Number(e.target.value) })}
            />
          </div>
        </section>

        <section className="context__block">
          <h3>Diagnostics and crash reports</h3>
          <p className="muted settings__help">
            Logs are written locally and never sent anywhere. There is no telemetry in this app
            and no code to transmit any of it — when something goes wrong, read the file, then
            decide for yourself whether to share it.
          </p>
          {diagnostics?.logDirectory && (
            <dl className="props">
              <div className="props__row">
                <dt>Log directory</dt>
                <dd className="muted">{diagnostics.logDirectory}</dd>
              </div>
            </dl>
          )}
          {crash ? (
            <>
              <p className="warning settings__help">
                A crash was recorded in an earlier run.
              </p>
              <pre className="settings__crash">{crash.excerpt}</pre>
              <p className="muted settings__help">From {crash.file}</p>
            </>
          ) : (
            <p className="muted settings__help">No crash recorded in the current log.</p>
          )}
        </section>

        <section className="context__block">
          <h3>Updates</h3>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={draft.checkUpdatesOnStartup}
              onChange={(e) => patch({ checkUpdatesOnStartup: e.target.checked })}
            />
            Check for a new release on startup
          </label>
          <p className="muted settings__help">
            On by default — the only network request the app makes of its own. Turn it off and
            everything it talks to is a cluster you chose.
          </p>
        </section>

        {diagnostics && (
          <section className="context__block">
            <h3>Diagnostics</h3>
            <dl className="props">
              <div className="props__row">
                <dt>Version</dt>
                <dd>{diagnostics.version}</dd>
              </div>
              <div className="props__row">
                <dt>Kubeconfig</dt>
                <dd>{diagnostics.kubeconfigPaths.join(", ") || "none found"}</dd>
              </div>
              <div className="props__row">
                <dt>Preferences file</dt>
                <dd className="muted">{diagnostics.preferencesPath ?? "unavailable"}</dd>
              </div>
              <div className="props__row">
                <dt>Active watches</dt>
                <dd>{diagnostics.activeWatches}</dd>
              </div>
              <div className="props__row">
                <dt>Connected clusters</dt>
                <dd>{diagnostics.connectedClusters.join(", ") || "none"}</dd>
              </div>
            </dl>
          </section>
        )}
      </div>

      <footer className="settings__footer">
        <button className="button button--primary" disabled={!dirty || busy} onClick={() => void save()}>
          {busy ? "Saving…" : "Save"}
        </button>
        <button className="button" disabled={!dirty} onClick={() => setDraft(preferences)}>
          Discard
        </button>
        {dirty && <span className="muted">unsaved changes</span>}
        {status && !dirty && <span className="muted">{status}</span>}
        {error && <span className="error">{error}</span>}
      </footer>
    </div>
  );
}

function StringList({
  values,
  placeholder,
  onChange,
}: {
  values: string[];
  placeholder?: string;
  onChange: (next: string[]) => void;
}) {
  return (
    <div className="kv">
      {values.map((value, index) => (
        <div className="kv__row" key={index}>
          <input
            value={value}
            placeholder={placeholder}
            onChange={(e) => {
              const next = values.slice();
              next[index] = e.target.value;
              onChange(next);
            }}
          />
          <button
            className="icon-button"
            onClick={() => onChange(values.filter((_, i) => i !== index))}
          >
            ✕
          </button>
        </div>
      ))}
      <button className="button button--ghost" onClick={() => onChange([...values, ""])}>
        + Add directory
      </button>
    </div>
  );
}
