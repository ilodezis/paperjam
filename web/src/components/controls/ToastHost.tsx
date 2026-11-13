import { AlertCircle, AlertTriangle, Check, Info, X } from "lucide-react";
import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";

import { dismissToast, getToastsSnapshot, subscribeToasts, type ToastIcon } from "./toast";

function ToastIconBadge({ icon }: { icon?: ToastIcon }) {
  switch (icon) {
    case "alert":
      return <AlertTriangle className="toast-icon toast-icon--alert" aria-hidden="true" />;
    case "error":
      return <AlertCircle className="toast-icon toast-icon--error" aria-hidden="true" />;
    case "info":
      return <Info className="toast-icon toast-icon--info" aria-hidden="true" />;
    case "check":
    default:
      return <Check className="toast-icon toast-icon--check" aria-hidden="true" />;
  }
}

export function ToastHost() {
  const { t } = useTranslation();
  const toasts = useSyncExternalStore(subscribeToasts, getToastsSnapshot, getToastsSnapshot);

  if (toasts.length === 0) return null;

  return (
    <div className="toast-host" role="region" aria-label={t("common.notifications")} aria-live="polite">
      {toasts.map((item) => (
        <div key={item.id} className={`toast-item toast-item--${item.icon ?? "check"}`} role="status">
          <div className="toast-content">
            <ToastIconBadge icon={item.icon} />
            <span className="toast-message">{item.message}</span>
            {item.undo ? (
              <button
                type="button"
                className="toast-undo"
                onClick={() => {
                  item.undo?.();
                  dismissToast(item.id);
                }}
              >
                {item.undoLabel ?? t("common.undo")}
              </button>
            ) : null}
            <button
              type="button"
              className="toast-dismiss"
              onClick={() => dismissToast(item.id)}
              aria-label={t("common.dismissNotification")}
            >
              <X aria-hidden="true" />
            </button>
          </div>
          {!item.persist ? (
            <div className="toast-progress-track">
              <span
                className="toast-progress-bar"
                style={{ animationDuration: `${item.duration}ms` }}
              />
            </div>
          ) : null}
        </div>
      ))}
    </div>
  );
}
