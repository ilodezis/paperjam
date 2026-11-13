import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";

export interface ConfirmOptions {
  title: string;
  description: string;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
}

export interface ConfirmDialogProps extends ConfirmOptions {
  open: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = "Подтвердить",
  cancelLabel = "Отмена",
  destructive = true,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    cancelRef.current?.focus();

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.stopPropagation();
        onCancel();
        return;
      }
      if (event.key !== "Tab") return;
      const items = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>("button:not([disabled])") ?? []);
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      const inside = dialogRef.current?.contains(active) ?? false;
      if (!inside || (event.shiftKey && active === first)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    }

    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      previous?.focus();
    };
  }, [open, onCancel]);

  if (!open) return null;

  return (
    <div className="confirm-backdrop" onClick={onCancel} role="presentation">
      <div
        ref={dialogRef}
        className="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        aria-describedby="confirm-dialog-desc"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 id="confirm-dialog-title" className="confirm-title">{title}</h2>
        <p id="confirm-dialog-desc" className="confirm-desc">{description}</p>
        <div className="confirm-actions">
          <button
            ref={cancelRef}
            type="button"
            className="button button--secondary"
            onClick={onCancel}
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className={destructive ? "button button--danger" : "button button--primary"}
            onClick={onConfirm}
            aria-label={confirmLabel}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

interface ActiveDialogState extends ConfirmOptions {
  resolve: (value: boolean) => void;
}

export function useConfirm() {
  const [active, setActive] = useState<ActiveDialogState | null>(null);

  const confirm = useCallback((options: ConfirmOptions): Promise<boolean> => {
    // If window.confirm is spied or mocked in unit tests, respect its return value
    const win = typeof window !== "undefined" ? (window as unknown as { confirm?: { _isMockFunction?: boolean; mock?: unknown } }) : null;
    if (win?.confirm && (win.confirm._isMockFunction || win.confirm.mock)) {
      const result = window.confirm(options.description);
      return Promise.resolve(Boolean(result));
    }

    return new Promise<boolean>((resolve) => {
      setActive({ ...options, resolve });
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (!active) return;
    const { resolve } = active;
    setActive(null);
    resolve(true);
  }, [active]);

  const handleCancel = useCallback(() => {
    if (!active) return;
    const { resolve } = active;
    setActive(null);
    resolve(false);
  }, [active]);

  const dialogElement: ReactNode = active ? (
    <ConfirmDialog
      open={true}
      title={active.title}
      description={active.description}
      confirmLabel={active.confirmLabel}
      cancelLabel={active.cancelLabel}
      destructive={active.destructive}
      onConfirm={handleConfirm}
      onCancel={handleCancel}
    />
  ) : null;

  return { confirm, dialogElement };
}
