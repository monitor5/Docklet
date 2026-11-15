import { useId, useState } from "react";
import { api, type Image } from "../../api";
import { ConfirmButton } from "../../components/ConfirmButton";
import { Empty } from "../../components/Empty";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useNotice } from "../../components/useNotice";
import { formatBytes, shortId } from "../../format";
import { useDockerList } from "../../hooks";
import { appendBounded, imageLabel } from "./pull";

export function ImagesView() {
  const list = useDockerList(api.images, true);
  const notice = useNotice();
  const [name, setName] = useState("");
  const [lines, setLines] = useState<string[]>([]);
  const inputId = useId();
  const pulling = notice.busy === "pull";

  const pull = () => {
    const target = name.trim();
    if (!target) return;
    setLines([]);
    notice.run("pull", async () => {
      await api.imagePull(target, (line) =>
        setLines((current) => appendBounded(current, line)),
      );
      setName("");
    });
  };

  return (
    <div className="list-pane">
      <form
        className="row pull"
        onSubmit={(event) => {
          event.preventDefault();
          pull();
        }}
      >
        <label htmlFor={inputId}>Pull</label>
        <input
          id={inputId}
          value={name}
          placeholder="nginx:latest"
          spellCheck={false}
          disabled={pulling}
          onChange={(event) => setName(event.target.value)}
        />
        <button
          type="submit"
          className="primary"
          disabled={pulling || !name.trim()}
        >
          {pulling ? "Pulling…" : "Pull"}
        </button>
      </form>
      <ErrorBanner
        message={notice.error ?? list.error}
        onDismiss={notice.error ? notice.dismiss : undefined}
      />
      {lines.length > 0 && (
        <ol className="progress" aria-live="polite" aria-label="Pull progress">
          {lines.map((line, index) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: append-only list
            <li key={index}>{line}</li>
          ))}
        </ol>
      )}
      {list.items.length === 0 ? (
        <Empty>No images.</Empty>
      ) : (
        <ul className="rows">
          {list.items.map((image) => (
            <ImageRow
              key={image.Id}
              image={image}
              busy={notice.busy === `rm:${image.Id}`}
              onRemove={() =>
                notice.run(`rm:${image.Id}`, () => api.imageRemove(image.Id))
              }
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function ImageRow({
  image,
  busy,
  onRemove,
}: {
  image: Image;
  busy: boolean;
  onRemove: () => void;
}) {
  const label = imageLabel(image);
  return (
    <li className="row-item images">
      <span className="name ellipsis" title={image.RepoTags?.join(", ")}>
        {label}
      </span>
      <code className="muted">{shortId(image.Id)}</code>
      <span className="muted">{formatBytes(image.Size)}</span>
      <span className="muted">
        {new Date(image.Created * 1000).toLocaleDateString()}
      </span>
      <span className="actions">
        <ConfirmButton
          label="Remove"
          ariaLabel={`Remove ${label}`}
          disabled={busy}
          onConfirm={onRemove}
        />
      </span>
    </li>
  );
}
