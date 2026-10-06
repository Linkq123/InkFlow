import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./pipeline";
import { collectImageDestinations } from "./image-destinations";
import { applyTextEdits, imagePathRewriteEdits } from "../document-state";

describe("HTML image attribute semantics", () => {
  it.each([
    "a&amp-b.png", "a&#38-b.png", "a&#x26-b.png", "a&copy-b.png",
    "a&amp=1.png", "a&notit.png", "a&#128;.png", "a&#0;.png", "a&NotEqualTilde;.png",
  ])("agrees with the rendered HTML and preserves rewrite ranges: %s", async spelling => {
    for (const attribute of ["src", "srcset"]) {
      const source = '<img ' + attribute + '="' + spelling + (attribute === "srcset" ? " 1x" : "") + '">';
      const html = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
      const rendered = html.querySelector("img")!.getAttribute(attribute)!;
      const destination = attribute === "srcset" ? rendered.slice(0, -3) : rendered;
      expect(collectImageDestinations(source).map(item => item.destination)).toEqual([destination]);
      const result = applyTextEdits(source, imagePathRewriteEdits(source, [{
        source: destination, destination: "Copy.assets/image.png",
      }]));
      expect(result).toBe('<img ' + attribute + '="Copy.assets/image.png' + (attribute === "srcset" ? " 1x" : "") + '">');
    }
  });
});
