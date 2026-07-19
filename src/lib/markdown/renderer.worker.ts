self.onmessage = async (event: MessageEvent<{ revision: number; markdown: string }>) => {
  const { revision, markdown } = event.data;
  try {
    const { renderMarkdown } = await import("./pipeline");
    const html = await renderMarkdown(markdown);
    self.postMessage({ revision, html });
  } catch (error) {
    self.postMessage({ revision, error: error instanceof Error ? error.message : String(error) });
  }
};
