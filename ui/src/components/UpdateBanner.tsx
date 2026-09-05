import { useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useStore } from "../store";

type Stage = "idle" | "checking" | "available" | "downloading" | "ready" | "error";

/**
 * Update prompt.
 *
 * Checking is opt-in and never automatic on first run: the app should make no
 * outbound request the user did not ask for. Installing is always an explicit
 * click — an editor with unsaved YAML must not be restarted underneath someone.
 */
export function UpdateBanner() {
  const preferences = useStore((s) => s.preferences);
  const [stage, setStage] = useState<Stage>("idle");
  const [update, setUpdate] = useState<Update | null>(null);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    if (!preferences?.checkUpdatesOnStartup || stage !== "idle") return;
    let cancelled = false;
    setStage("checking");
    setError(null);

    void check()
      .then((found) => {
        if (cancelled) return;
        if (found) {
          setUpdate(found);
          setStage("available");
        } else {
          setStage("idle");
        }
      })
      .catch((err) => {
        if (cancelled) return;
        // Stage goes back to "idle" — nothing is downloading — but the error
        // itself has to survive that, or a failed check is indistinguishable
        // from "no update" and no one would ever know to look at Settings.
        setError(String(err));
        setStage("idle");
      });

    return () => {
      cancelled = true;
    };
  }, [preferences?.checkUpdatesOnStartup]);

  if (dismissed || stage === "checking") return null;

  if (!update) {
    if (!error) return null;
    return (
      <div className="banner banner--update banner--warn">
        <span>Could not check for updates: {error}</span>
        <button className="icon-button" onClick={() => setDismissed(true)} title="Dismiss">
          ✕
        </button>
      </div>
    );
  }

  const install = async () => {
    setStage("downloading");
    setError(null);
    try {
      let total = 0;
      let received = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          setProgress(total > 0 ? (received / total) * 100 : 0);
        }
      });
      setStage("ready");
    } catch (err) {
      setError(String(err));
      setStage("error");
    }
  };

  return (
    <div className="banner banner--update">
      <span>
        Version {update.version} is available.
        {stage === "downloading" && ` Downloading… ${progress.toFixed(0)}%`}
        {stage === "ready" && " Installed — restart to use it."}
        {error && ` ${error}`}
      </span>

      {stage === "available" && (
        <button className="button" onClick={() => void install()}>
          Download and install
        </button>
      )}
      {stage === "ready" && (
        <button className="button button--primary" onClick={() => void relaunch()}>
          Restart now
        </button>
      )}
      <button className="icon-button" onClick={() => setDismissed(true)} title="Dismiss">
        ✕
      </button>
    </div>
  );
}
