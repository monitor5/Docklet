import { useVirtualizer } from "@tanstack/react-virtual";
import { useLayoutEffect, useRef, useState } from "react";
import type { Container } from "../../api";
import { ErrorBanner } from "../../components/ErrorBanner";
import { containerName } from "../../format";
import { stripAnsi } from "./ansi";
import { useLogStream } from "./useLogStream";

const ROW_HEIGHT = 18;
/** Distance from the bottom that still counts as "at the bottom". */
const STICK_THRESHOLD = 24;

type Props = {
  container: Container;
  onClose: () => void;
};

export function LogView({ container, onClose }: Props) {
  const { buffer, version, error } = useLogStream(container.Id);
  const scroller = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const [stuck, setStuck] = useState(true);

  const virtualizer = useVirtualizer({
    count: buffer.size,
    getScrollElement: () => scroller.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 30,
    getItemKey: (index) => buffer.firstIndex + index,
  });

  // `version` is the signal that the buffer changed.
  // biome-ignore lint/correctness/useExhaustiveDependencies: version drives the effect
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element && stickRef.current) element.scrollTop = element.scrollHeight;
  }, [version]);

  const onScroll = () => {
    const element = scroller.current;
    if (!element) return;
    const atBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <
      STICK_THRESHOLD;
    stickRef.current = atBottom;
    setStuck(atBottom);
  };

  const jumpToBottom = () => {
    const element = scroller.current;
    stickRef.current = true;
    setStuck(true);
    if (element) element.scrollTop = element.scrollHeight;
  };

  return (
    <section
      className="logs"
      aria-label={`Logs of ${containerName(container)}`}
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
    >
      <header className="logs-header">
        <strong className="ellipsis">{containerName(container)}</strong>
        <span className="muted">{buffer.size.toLocaleString()} lines</span>
        {!stuck && (
          <button type="button" onClick={jumpToBottom}>
            Jump to bottom
          </button>
        )}
        <button type="button" className="ghost" onClick={onClose}>
          Close
        </button>
      </header>
      <ErrorBanner message={error} />
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: keyboard scrolling of the log */}
      <div
        className="log-scroll"
        ref={scroller}
        onScroll={onScroll}
        tabIndex={0}
        role="log"
        aria-live="off"
        aria-label="Log output"
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => (
            <div
              key={item.key}
              className="log-line"
              style={{ transform: `translateY(${item.start}px)` }}
            >
              {stripAnsi(buffer.get(item.index) ?? "")}
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
