import { api } from "../../api";
import { ConfirmButton } from "../../components/ConfirmButton";
import { Empty } from "../../components/Empty";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useNotice } from "../../components/useNotice";
import { useDockerList } from "../../hooks";

export function NetworksView() {
  const list = useDockerList(api.networks, true);
  const notice = useNotice();

  return (
    <div className="list-pane">
      <ErrorBanner
        message={notice.error ?? list.error}
        onDismiss={notice.error ? notice.dismiss : undefined}
      />
      {list.items.length === 0 ? (
        <Empty>No networks.</Empty>
      ) : (
        <ul className="rows">
          {list.items.map((network, index) => {
            const id = network.Id ?? network.Name ?? String(index);
            const name = network.Name ?? id.slice(0, 12);
            return (
              <li className="row-item networks" key={id}>
                <span className="name ellipsis" title={name}>
                  {name}
                </span>
                <span className="muted">{network.Driver}</span>
                <span className="muted">{network.Scope}</span>
                <span className="actions">
                  <ConfirmButton
                    label="Remove"
                    ariaLabel={`Remove ${name}`}
                    disabled={notice.busy.has(id) || !network.Id}
                    onConfirm={() =>
                      notice.run(id, () =>
                        api.networkRemove(network.Id as string),
                      )
                    }
                  />
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
