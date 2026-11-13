export type ToastIcon = "check" | "info" | "alert" | "error";

export interface ToastOptions {
  icon?: ToastIcon;
  undo?: () => void;
  undoLabel?: string;
  duration?: number;
  persist?: boolean;
}

export interface ToastItem extends ToastOptions {
  id: string;
  message: string;
  createdAt: number;
}

let toasts: ToastItem[] = [];
const listeners = new Set<() => void>();

function notify() {
  for (const listener of listeners) {
    listener();
  }
}

export function subscribeToasts(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getToastsSnapshot(): ToastItem[] {
  return toasts;
}

let counter = 0;

export function toast(message: string, options: ToastOptions = {}): string {
  const id = `toast-${++counter}-${Date.now()}`;
  const duration = options.duration ?? 3800;
  const item: ToastItem = {
    id,
    message,
    icon: options.icon ?? "check",
    undo: options.undo,
    undoLabel: options.undoLabel,
    duration,
    persist: options.persist,
    createdAt: Date.now(),
  };

  toasts = [...toasts, item];
  notify();

  if (!item.persist) {
    setTimeout(() => {
      dismissToast(id);
    }, duration);
  }

  return id;
}

export function dismissToast(id: string): void {
  toasts = toasts.filter((item) => item.id !== id);
  notify();
}

export function clearToasts(): void {
  toasts = [];
  notify();
}
