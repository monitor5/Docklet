import type { ReactNode } from "react";

type Props = {
  title: string;
  detail?: string;
  /** Shows an indeterminate indicator. */
  busy?: boolean;
  children?: ReactNode;
};

/** Stopped, starting/stopping and error screens share this shell. */
export function EngineMessage({ title, detail, busy, children }: Props) {
  return (
    <section className="panel" aria-live="polite">
      <h2>{title}</h2>
      {busy && <div className="bar" role="progressbar" aria-label={title} />}
      {detail && <p className="detail">{detail}</p>}
      {children && <div className="row">{children}</div>}
    </section>
  );
}
