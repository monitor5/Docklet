type Props = {
  message: string | null | undefined;
  onDismiss?: () => void;
};

/** Always rendered so the live region exists before the first error. */
export function ErrorBanner({ message, onDismiss }: Props) {
  return (
    <div role="alert" aria-live="assertive">
      {message ? (
        <div className="banner">
          <span className="banner-text">{message}</span>
          {onDismiss && (
            <button type="button" className="ghost" onClick={onDismiss}>
              Dismiss
            </button>
          )}
        </div>
      ) : null}
    </div>
  );
}
