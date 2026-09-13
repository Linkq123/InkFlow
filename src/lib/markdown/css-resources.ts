import parse from "css-tree/parser";
import generate from "css-tree/generator";
import walk from "css-tree/walker";
import { ident } from "css-tree/utils";
import type { CssNode, ListItem, List } from "css-tree";

const resourceFunctions = new Set(["url", "src", "image", "image-set", "-webkit-image-set", "cross-fade", "-webkit-cross-fade"]);

/** Parse CSS before it enters the live document. Unknown values fail closed. */
export function sanitizeResourceCss(source: string, inline = false): string {
  try {
    const ast = parse(source, { context: inline ? "declarationList" : "stylesheet", parseCustomProperty: true });
    walk(ast, {
      enter(node: CssNode, item: ListItem<CssNode> | null, list: List<CssNode> | null) {
        if (node.type === "Atrule" && ident.decode(node.name).toLowerCase() === "import") {
          if (item && list) list.remove(item);
          return walk.skip;
        }
        if (node.type !== "Declaration") return;
        let unsafe = false;
        walk(node.value, value => {
          if (value.type === "Raw") unsafe = true;
          if (value.type === "Url" && !/^(?:#|data:|blob:)/i.test(value.value.trim())) unsafe = true;
          if (value.type === "Function" && resourceFunctions.has(ident.decode(value.name).toLowerCase())) unsafe = true;
        });
        if (unsafe && item && list) list.remove(item);
        return walk.skip;
      },
    });
    return generate(ast);
  } catch { return ""; }
}

/** Discovery only. The renderer CSP and output sanitizer enforce the policy. */
export function hasRemoteCssReference(source: string): boolean {
  const decoded = source.replace(/\\([\da-f]{1,6})[\t\n\r\f ]?|\\([^\r\n\f])/gi, (_, hex: string, char: string) =>
    hex ? String.fromCodePoint(Math.min(Number.parseInt(hex, 16), 0x10ffff)) : char);
  // Scan disjoint regions, including unterminated comments/functions. A greedy
  // regex starting at each `url(` would repeatedly scan malformed suffixes.
  const parts: string[] = [];
  let cursor = 0;
  for (;;) {
    const start = decoded.indexOf("/*", cursor);
    if (start < 0) { parts.push(decoded.slice(cursor)); break; }
    parts.push(decoded.slice(cursor, start));
    const end = decoded.indexOf("*/", start + 2);
    if (end < 0) break;
    cursor = end + 2;
  }
  const css = parts.join("");
  const functions = /(?:url|src|image-set|-webkit-image-set)\s*\(/gi;
  for (let match; (match = functions.exec(css));) {
    let depth = 1, quote = "", end = functions.lastIndex;
    const start = end;
    for (; end < css.length && depth; end++) {
      const char = css[end];
      if (quote) { if (char === quote) quote = ""; }
      else if (char === '"' || char === "'") quote = char;
      else if (char === "(") depth++;
      else if (char === ")") depth--;
    }
    if (/(?:https?:|\/\/)/i.test(css.slice(start, end))) return true;
    functions.lastIndex = end;
  }
  return false;
}
