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
        // A failed check is not worth interrupting anyone; it is reported only
        // if they went looking.
        setError(String(err));
        setStage("idle");
      });

    return () => {
      cancelled = true;
    };
  }, [preferences?.checkUpdatesOnStartup]);

  if (!update || dismissed || stage === "idle" || stage === "checking") return null;

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
