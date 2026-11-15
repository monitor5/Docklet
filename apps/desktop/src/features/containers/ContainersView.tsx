import { useMemo, useState } from "react";
import { api, type Container } from "../../api";
import { Empty } from "../../components/Empty";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useNotice } from "../../components/useNotice";
import { useDockerList } from "../../hooks";
import { LogView } from "../logs/LogView";
import { ContainerGroup } from "./ContainerGroup";
import { ContainerRow } from "./ContainerRow";
import { groupContainers } from "./grouping";

export function ContainersView() {
  const list = useDockerList(api.containers, true);
  const notice = useNotice();
  const [selected, setSelected] = useState<Container | null>(null);
  const entries = useMemo(() => groupContainers(list.items), [list.items]);

  const rowProps = (container: Container) => ({
    container,
    busy: notice.busy?.startsWith(`${container.Id}:`) ?? false,
    selected: selected?.Id === container.Id,
    onSelect: () => setSelected(container),
    onAction: (action: Parameters<typeof api.containerAction>[1]) =>
      notice.run(`${container.Id}:${action}`, async () => {
        await api.containerAction(container.Id, action);
        if (action === "remove" && selected?.Id === container.Id) {
          setSelected(null);
        }
      }),
  });

  return (
    <div className={selected ? "split" : undefined}>
      <div className="list-pane">
        <ErrorBanner
          message={notice.error ?? list.error}
          onDismiss={notice.error ? notice.dismiss : undefined}
        />
        {entries.length === 0 ? (
          <Empty>No containers.</Empty>
        ) : (
          <ul className="rows">
            {entries.map((entry) =>
              entry.kind === "group" ? (
                <ContainerGroup
                  key={`group:${entry.project}`}
                  project={entry.project}
                  count={entry.containers.length}
                >
                  {entry.containers.map((container) => (
                    <ContainerRow key={container.Id} {...rowProps(container)} />
                  ))}
                </ContainerGroup>
              ) : (
                <ContainerRow
                  key={entry.container.Id}
                  {...rowProps(entry.container)}
                />
              ),
            )}
          </ul>
        )}
      </div>
      {selected && (
        <LogView
          // A new key gives each container its own stream and buffer.
          key={selected.Id}
          container={selected}
          onClose={() => setSelected(null)}
        />
      )}
    </div>
  );
}
