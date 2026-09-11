/** Serialize a label and the already URL-encoded path returned by write_asset. */
export function serializeMarkdownImage(label: string, markdownPath: string): string {
  const alt = label
    .replace(/[\r\n]/g, " ")
    .replace(/[!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~]/g, "\\$&");
  // Preserve existing percent escapes. Encode syntax delimiters and controls,
  // and use angle brackets when spaces or parentheses require them.
  const path = markdownPath.replace(/[\\<>\u0000-\u001f\u007f]/g, character =>
    `%${character.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`
  );
  const destination = /[\s()]/.test(path) ? `<${path}>` : path;
  return `![${alt}](${destination})`;
}
