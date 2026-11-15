import type { Container, ContainerAction } from "../../api";
import { ConfirmButton } from "../../components/ConfirmButton";
import { containerName, publishedPorts } from "../../format";
import { isRunning } from "./grouping";

type Props = {
  container: Container;
  busy: boolean;
  selected: boolean;
  onSelect: () => void;
  onOpenPort: (port: number) => void;
  onAction: (action: ContainerAction) => void;
};

export function ContainerRow({
  container,
  busy,
  selected,
  onSelect,
  onOpenPort,
  onAction,
}: Props) {
  const name = containerName(container);
  const running = isRunning(container);
  const ports = publishedPorts(container.Ports);

  return (
    <li className={selected ? "row-item selected" : "row-item"}>
      <span
        className={`dot ${running ? "on" : "off"}`}
        role="img"
        aria-label={container.State ?? "unknown"}
      />
      <button
        type="button"
        className="name link-like"
        aria-pressed={selected}
        title="Show logs"
        onClick={onSelect}
      >
        {name}
      </button>
      <span className="muted ellipsis" title={container.Image}>
        {container.Image}
      </span>
      <span className="muted">{container.Status ?? container.State}</span>
      <span className="ports">
        {ports.map((port) => (
          <button
            key={port}
            type="button"
            className="link-like"
            title={`Open http://localhost:${port}`}
            onClick={() => onOpenPort(port)}
          >
            :{port}
          </button>
        ))}
      </span>
      <span className="actions">
        {running ? (
          <>
            <button
              type="button"
              className="ghost"
              disabled={busy}
              aria-label={`Stop ${name}`}
              onClick={() => onAction("stop")}
            >
              Stop
            </button>
            <button
              type="button"
              className="ghost"
              disabled={busy}
              aria-label={`Restart ${name}`}
              onClick={() => onAction("restart")}
            >
              Restart
            </button>
          </>
        ) : (
          <button
            type="button"
            className="ghost"
            disabled={busy}
            aria-label={`Start ${name}`}
            onClick={() => onAction("start")}
          >
            Start
          </button>
        )}
        <ConfirmButton
          label="Remove"
          ariaLabel={`Remove ${name}`}
          disabled={busy}
          onConfirm={() => onAction("remove")}
        />
      </span>
    </li>
  );
}
