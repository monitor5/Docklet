import type { ReactNode } from "react";

type Props = {
  title: string;
  detail?: string;
  /** What the actions below are for. */
  hint?: string;
  /** Shows an indeterminate indicator. */
  busy?: boolean;
  children?: ReactNode;
};

/** Stopped, sleeping, starting/stopping and error screens share this shell. */
export function EngineMessage({ title, detail, hint, busy, children }: Props) {
  return (
    <section className="panel" aria-live="polite">
      <h2>{title}</h2>
      {busy && <div className="bar" role="progressbar" aria-label={title} />}
      {detail && <p className="detail">{detail}</p>}
      {hint && <p className="muted">{hint}</p>}
      {children && <div className="row">{children}</div>}
    </section>
  );
}
