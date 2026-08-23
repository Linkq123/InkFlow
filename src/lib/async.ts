/** Wait for a best-effort prerequisite without allowing it to block the UI forever. */
export async function waitForPromiseOrTimeout(
  prerequisite: PromiseLike<unknown>,
  timeoutMs: number,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, Math.max(0, timeoutMs));
  });

  try {
    await Promise.race([
      Promise.resolve(prerequisite).then(() => undefined, () => undefined),
      timeout,
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** Wait for dynamically inserted images to decode without blocking export forever. */
export async function waitForImagesOrTimeout(
  root: ParentNode,
  timeoutMs: number,
): Promise<void> {
  const cleanups: Array<() => void> = [];
  const images = Array.from(root.querySelectorAll<HTMLImageElement>("img"));
  const ownerDocument = root instanceof Document
    ? root
    : (root as Node).ownerDocument;
  if (ownerDocument) {
    for (const image of Array.from(
      root.querySelectorAll<SVGImageElement>("svg image"),
    )) {
      const source = image.getAttribute("href")
        ?? image.getAttribute("xlink:href")
        ?? "";
      if (!source || source.startsWith("#")) continue;
      // SVGImageElement has no portable decode() API. Preloading the same URL
      // through an HTML image uses the browser cache and gives us a bounded,
      // observable decode promise before WebView2 starts printing.
      const preload = ownerDocument.createElement("img");
      preload.src = source;
      images.push(preload);
    }
  }
  const pending = images.map((image) => {
    if (typeof image.decode === "function") {
      try {
        return image.decode().then(
          () => undefined,
          () => undefined,
        );
      } catch {
        return Promise.resolve();
      }
    }
    if (image.complete) return Promise.resolve();
    return new Promise<void>((resolve) => {
      let settled = false;
      const cleanup = () => {
        image.removeEventListener("load", finish);
        image.removeEventListener("error", finish);
      };
      const finish = () => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve();
      };
      cleanups.push(cleanup);
      image.addEventListener("load", finish);
      image.addEventListener("error", finish);
      // The image can finish between the initial check and listener setup.
      if (image.complete) finish();
    });
  });

  try {
    await waitForPromiseOrTimeout(Promise.all(pending), timeoutMs);
  } finally {
    for (const cleanup of cleanups) cleanup();
  }
}
