import { useState } from "react";

type Props = {
  label: string;
  /** Names the target for assistive technology, e.g. "Remove web". */
  ariaLabel?: string;
  disabled?: boolean;
  onConfirm: () => void;
};

/** A destructive action that needs a second click to go through. */
export function ConfirmButton({
  label,
  ariaLabel,
  disabled,
  onConfirm,
}: Props) {
  const [asking, setAsking] = useState(false);

  if (!asking) {
    return (
      <button
        type="button"
        className="ghost danger"
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setAsking(true)}
      >
        {label}
      </button>
    );
  }
  return (
    <fieldset
      className="confirm"
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) {
          setAsking(false);
        }
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") setAsking(false);
      }}
    >
      <button
        type="button"
        className="danger solid"
        // biome-ignore lint/a11y/noAutofocus: focus moves into the confirmation it just opened
        autoFocus
        onClick={() => {
          setAsking(false);
          onConfirm();
        }}
      >
        Confirm
      </button>
      <button type="button" className="ghost" onClick={() => setAsking(false)}>
        Cancel
      </button>
    </fieldset>
  );
}
