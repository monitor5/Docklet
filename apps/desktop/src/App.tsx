import { ENGINE_STATE_LABEL, type EngineState } from "./engine";

const state: EngineState = "not_installed";

export default function App() {
  return (
    <main className="shell">
      <h1>Docklet</h1>
      <p className="status">Engine: {ENGINE_STATE_LABEL[state]}</p>
    </main>
  );
}
