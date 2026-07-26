self.onmessage = async (event: MessageEvent<{
  revision: number;
  markdown: string;
  operation?: "render" | "detectRemoteImages" | "analyze";
}>) => {
  const { revision, markdown, operation = "render" } = event.data;
  try {
    if (operation === "detectRemoteImages") {
      const { hasRemoteImages } = await import("./resources");
      self.postMessage({
        revision,
        hasRemoteImages: await hasRemoteImages(markdown),
      });
      return;
    }
    if (operation === "analyze") {
      const [{ documentStats, extractOutline }, { hasRemoteImages }] = await Promise.all([
        import("../stats"),
        import("./resources"),
      ]);
      self.postMessage({
        revision,
        analysis: {
          stats: documentStats(markdown),
          outline: extractOutline(markdown),
          hasRemoteImages: await hasRemoteImages(markdown),
        },
      });
      return;
    }
    const { renderMarkdown } = await import("./pipeline");
    const html = await renderMarkdown(markdown);
    self.postMessage({ revision, html });
  } catch (error) {
    self.postMessage({ revision, error: error instanceof Error ? error.message : String(error) });
  }
};
