import { type ReactNode, useId, useState } from "react";

type Props = {
  project: string;
  count: number;
  children: ReactNode;
};

export function ContainerGroup({ project, count, children }: Props) {
  const [open, setOpen] = useState(true);
  const bodyId = useId();
  return (
    <li className="group">
      <button
        type="button"
        className="group-header"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="chevron" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        <span className="group-name">{project}</span>
        <span className="muted">{count}</span>
      </button>
      <ul className="rows nested" id={bodyId} hidden={!open}>
        {children}
      </ul>
    </li>
  );
}
