import { useState } from "react";
import { api } from "./api";
import { ErrorBanner } from "./components/ErrorBanner";
import { useNotice } from "./components/useNotice";
import { ENGINE_STATE_LABEL } from "./engine";
import { ContainersView } from "./features/containers/ContainersView";
import { EngineMessage } from "./features/engine/EngineMessage";
import { InstallScreen } from "./features/engine/InstallScreen";
import { screenFor } from "./features/engine/screen";
import { WslMissing } from "./features/engine/WslMissing";
import { ImagesView } from "./features/images/ImagesView";
import { NetworksView } from "./features/networks/NetworksView";
import { VolumesView } from "./features/volumes/VolumesView";
import { useEngine } from "./hooks";

const TABS = [
  { id: "containers", label: "Containers", View: ContainersView },
  { id: "images", label: "Images", View: ImagesView },
  { id: "volumes", label: "Volumes", View: VolumesView },
  { id: "networks", label: "Networks", View: NetworksView },
] as const;

type TabId = (typeof TABS)[number]["id"];

export default function App() {
  const engine = useEngine();
  const notice = useNotice();
  const [tab, setTab] = useState<TabId>("containers");
  const [mirroredDismissed, setMirroredDismissed] = useState(false);
  const { status } = engine;

  if (!status) {
    return (
      <main className="shell">
        <EngineMessage title="Connecting…" busy />
        <ErrorBanner message={engine.error} onDismiss={engine.dismissError} />
      </main>
    );
  }

  const control = (key: string, task: () => Promise<unknown>) =>
    notice.run(key, async () => {
      try {
        await task();
      } finally {
        await engine.refresh();
      }
    });

  const screen = screenFor(status.state);
  // One engine request at a time; the backend would refuse a second one.
  const acting = notice.busy.has("start") || notice.busy.has("stop");
  const errors = (
    <ErrorBanner
      message={notice.error ?? engine.error}
      onDismiss={() => {
        notice.dismiss();
        engine.dismissError();
      }}
    />
  );

  const onTabKey = (event: React.KeyboardEvent<HTMLElement>) => {
    const step =
      event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    const index = TABS.findIndex((item) => item.id === tab);
    const next = TABS[(index + step + TABS.length) % TABS.length];
    setTab(next.id);
    document.getElementById(`tab-${next.id}`)?.focus();
  };

  return (
    <main className="shell">
      <header className="top">
        <h1>Docklet</h1>
        <span className={`pill ${status.state}`}>
          {ENGINE_STATE_LABEL[status.state]}
        </span>
        {screen === "running" && (
          <button
            type="button"
            className="ghost push"
            disabled={acting}
            onClick={() => control("stop", api.engineStop)}
          >
            Stop engine
          </button>
        )}
      </header>
      {errors}
      {status.mirrored_networking && !mirroredDismissed && (
        <div className="banner warning" role="status">
          <span className="banner-text">
            Your .wslconfig sets networkingMode=mirrored. Published container
            ports rely on WSL's NAT localhost forwarding and may not be
            reachable from Windows.
          </span>
          <button
            type="button"
            className="ghost"
            onClick={() => setMirroredDismissed(true)}
          >
            Dismiss
          </button>
        </div>
      )}
      {screen === "wsl_missing" && <WslMissing onRecheck={engine.refresh} />}
      {screen === "install" && (
        <InstallScreen bases={status.bases} onDone={engine.refresh} />
      )}
      {screen === "stopped" && (
        <EngineMessage title="The engine is stopped">
          <button
            type="button"
            className="primary"
            disabled={acting}
            onClick={() => control("start", api.engineStart)}
          >
            Start engine
          </button>
        </EngineMessage>
      )}
      {screen === "busy" && (
        <EngineMessage
          title={
            status.state === "starting"
              ? "Starting the engine…"
              : "Stopping the engine…"
          }
          hint="If this does not finish, start the engine again to repair it, or stop it."
          busy
        >
          {/* Without these the window could only wait: Docker up with the
              pipe proxy down, or a dockerd that never gets ready, both
              show as starting for good. */}
          <button
            type="button"
            disabled={acting}
            onClick={() => control("start", api.engineStart)}
          >
            Start engine
          </button>
          <button
            type="button"
            disabled={acting}
            onClick={() => control("stop", api.engineStop)}
          >
            Stop engine
          </button>
        </EngineMessage>
      )}
      {screen === "error" && (
        <EngineMessage
          title="The engine hit a problem"
          detail={status.reason ?? "Unknown error."}
        >
          <button type="button" className="primary" onClick={engine.refresh}>
            Retry
          </button>
        </EngineMessage>
      )}
      {screen === "running" && (
        <>
          <div
            className="tabs"
            role="tablist"
            aria-label="Resources"
            tabIndex={-1}
            onKeyDown={onTabKey}
          >
            {TABS.map((item) => (
              <button
                key={item.id}
                id={`tab-${item.id}`}
                type="button"
                role="tab"
                aria-selected={tab === item.id}
                aria-controls="tabpanel"
                tabIndex={tab === item.id ? 0 : -1}
                onClick={() => setTab(item.id)}
              >
                {item.label}
              </button>
            ))}
          </div>
          <div id="tabpanel" role="tabpanel" aria-labelledby={`tab-${tab}`}>
            {TABS.map((item) =>
              tab === item.id ? <item.View key={item.id} /> : null,
            )}
          </div>
        </>
      )}
    </main>
  );
}
