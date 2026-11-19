import { api } from "../../api";
import { ConfirmButton } from "../../components/ConfirmButton";
import { Empty } from "../../components/Empty";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useNotice } from "../../components/useNotice";
import { useDockerList } from "../../hooks";

export function VolumesView() {
  const list = useDockerList(api.volumes, true);
  const notice = useNotice();

  return (
    <div className="list-pane">
      <ErrorBanner
        message={notice.error ?? list.error}
        onDismiss={notice.error ? notice.dismiss : undefined}
      />
      {list.items.length === 0 ? (
        <Empty>No volumes.</Empty>
      ) : (
        <ul className="rows">
          {list.items.map((volume) => (
            <li className="row-item volumes" key={volume.Name}>
              <span className="name ellipsis" title={volume.Name}>
                {volume.Name}
              </span>
              <span className="muted">{volume.Driver}</span>
              <span className="actions">
                <ConfirmButton
                  label="Remove"
                  ariaLabel={`Remove ${volume.Name}`}
                  disabled={notice.busy.has(volume.Name)}
                  onConfirm={() =>
                    notice.run(volume.Name, () => api.volumeRemove(volume.Name))
                  }
                />
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
