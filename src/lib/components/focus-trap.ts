export interface FocusTrapOptions {
  onClose: () => void;
  initialFocus?: string;
}

const FOCUSABLE = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[href]",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function focusTrap(node: HTMLElement, options: FocusTrapOptions) {
  let current = options;
  const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  const focusInitial = () => {
    const preferred = current.initialFocus
      ? node.querySelector<HTMLElement>(current.initialFocus)
      : null;
    (preferred ?? node.querySelector<HTMLElement>(FOCUSABLE) ?? node).focus();
  };
  queueMicrotask(focusInitial);

  const keydown = (event: KeyboardEvent) => {
    if (event.isComposing) return;
    if (event.key === "Escape") {
      event.preventDefault();
      current.onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE))
      .filter((element) => element.offsetParent !== null);
    if (!focusable.length) {
      event.preventDefault();
      node.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1)!;
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };
  node.addEventListener("keydown", keydown);

  return {
    update(next: FocusTrapOptions) {
      current = next;
    },
    destroy() {
      node.removeEventListener("keydown", keydown);
      if (previous?.isConnected) previous.focus();
    },
  };
}
