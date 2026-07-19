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
