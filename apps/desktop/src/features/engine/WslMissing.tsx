import { CopyButton } from "../../components/CopyButton";
import { WSL_INSTALL_COMMAND } from "./screen";

export function WslMissing({ onRecheck }: { onRecheck: () => void }) {
  return (
    <section className="panel">
      <h2>WSL is required</h2>
      <p>
        Docklet runs Docker inside the Windows Subsystem for Linux. Run this in
        a terminal started as administrator, restart Windows if asked, then
        check again.
      </p>
      <div className="command">
        <code>{WSL_INSTALL_COMMAND}</code>
        <CopyButton text={WSL_INSTALL_COMMAND} />
      </div>
      <div className="row">
        <button type="button" className="primary" onClick={onRecheck}>
          Check again
        </button>
      </div>
    </section>
  );
}
