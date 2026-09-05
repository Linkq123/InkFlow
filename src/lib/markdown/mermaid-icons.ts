import type { SyncIconLoader } from "mermaid";

const inkflowIconPack: SyncIconLoader = {
  name: "inkflow",
  icons: {
    prefix: "inkflow",
    width: 24,
    height: 24,
    icons: {
      document: {
        body: '<path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 2h8l4 4v16H6zM14 2v5h5M9 12h6M9 16h6"/>',
      },
    },
  },
};

// Mermaid does not ship Iconify packs. Keep documented compatibility icons
// offline so diagram rendering cannot become an implicit network request.
const logosIconPack: SyncIconLoader = {
  name: "logos",
  icons: {
    prefix: "logos",
    width: 24,
    height: 24,
    icons: {
      "github-icon": {
        body: '<path fill="currentColor" d="M12 .7A11.5 11.5 0 0 0 8.36 23.1c.58.1.79-.25.79-.56v-2.23c-3.22.7-3.9-1.37-3.9-1.37-.53-1.34-1.29-1.7-1.29-1.7-1.05-.72.08-.7.08-.7 1.16.08 1.78 1.2 1.78 1.2 1.04 1.77 2.72 1.26 3.38.96.1-.75.4-1.26.74-1.55-2.57-.29-5.27-1.28-5.27-5.68 0-1.25.45-2.28 1.19-3.08-.12-.29-.52-1.46.11-3.04 0 0 .97-.31 3.16 1.18A10.98 10.98 0 0 1 12 6.14c.98 0 1.94.13 2.84.39 2.2-1.49 3.16-1.18 3.16-1.18.63 1.58.23 2.75.11 3.04.74.8 1.19 1.83 1.19 3.08 0 4.42-2.71 5.38-5.29 5.67.42.36.79 1.07.79 2.16v3.24c0 .31.21.67.8.56A11.5 11.5 0 0 0 12 .7Z"/>',
      },
    },
  },
};

export const bundledMermaidIconPacks: SyncIconLoader[] = [
  inkflowIconPack,
  logosIconPack,
];
