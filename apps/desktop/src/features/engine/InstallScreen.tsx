import { useId, useState } from "react";
import { api } from "../../api";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useNotice } from "../../components/useNotice";

type Props = {
  bases: string[];
  onDone: () => void;
};

export function InstallScreen({ bases, onDone }: Props) {
  const [base, setBase] = useState<string | undefined>(bases[0]);
  const [lines, setLines] = useState<string[]>([]);
  const notice = useNotice();
  const selectId = useId();
  const installing = notice.busy === "install";

  const install = () =>
    notice.run("install", async () => {
      setLines([]);
      try {
        await api.engineInstall(base, (line) =>
          setLines((current) => [...current, line]),
        );
      } finally {
        onDone();
      }
    });

  return (
    <section className="panel">
      <h2>Set up the Docker engine</h2>
      <p>
        Docklet installs Docker Engine into its own WSL distro. Other distros
        are not touched.
      </p>
      <div className="row">
        <label htmlFor={selectId}>Base</label>
        <select
          id={selectId}
          value={base ?? ""}
          disabled={installing || bases.length === 0}
          onChange={(event) => setBase(event.target.value)}
        >
          {bases.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="primary"
          disabled={installing}
          onClick={install}
        >
          {installing ? "Installing…" : "Install"}
        </button>
      </div>
      <ErrorBanner message={notice.error} onDismiss={notice.dismiss} />
      <ol className="progress" aria-live="polite" aria-label="Install progress">
        {lines.map((line, index) => (
          // Lines are append-only, so the index is a stable key.
          // biome-ignore lint/suspicious/noArrayIndexKey: append-only list
          <li key={index}>{line}</li>
        ))}
      </ol>
    </section>
  );
}
