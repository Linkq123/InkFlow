export interface PaletteCommand {
  id: string;
  label: string;
  shortcut?: string;
  section?: string;
  run: () => void | Promise<void>;
}

