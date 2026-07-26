import { commonmarkLanguage } from "@codemirror/lang-markdown";
import { decodeHTMLStrict } from "entities/decode";

export interface ImageDestination {
  raw: string;
  destination: string;
  from: number;
  to: number;
  syntax: "markdown" | "html";
}

const htmlImagePattern =
  /(<img\b[^>\r\n]*?\bsrc\s*=\s*["'])([^"'\r\n]+)(["'][^>]*>)/gi;

export function collectImageDestinations(markdown: string): ImageDestination[] {
  const destinations: ImageDestination[] = [];
  const referenceLabels = new Set<string>();
  const referenceDefinitions = new Map<string, ImageDestination>();
  const htmlRanges: Array<{ from: number; to: number }> = [];
  const tree = commonmarkLanguage.parser.parse(markdown);

  tree.iterate({
    enter(node) {
      if (node.name === "Image") {
        const url = node.node.getChild("URL");
        if (url) {
          destinations.push(markdownDestination(markdown, url.from, url.to));
          return;
        }

        const marks = node.node.getChildren("LinkMark");
        if (marks.length < 2) return;
        const alt = markdown.slice(marks[0].to, marks[1].from);
        const labelNode = node.node.getChild("LinkLabel");
        const explicitLabel = labelNode
          ? stripLabelBrackets(markdown.slice(labelNode.from, labelNode.to))
          : "";
        referenceLabels.add(normalizeReferenceLabel(explicitLabel || alt));
        return;
      }

      if (node.name === "LinkReference") {
        const label = node.node.getChild("LinkLabel");
        const url = node.node.getChild("URL");
        if (!label || !url) return;
        const normalized = normalizeReferenceLabel(
          stripLabelBrackets(markdown.slice(label.from, label.to)),
        );
        if (!referenceDefinitions.has(normalized)) {
          referenceDefinitions.set(
            normalized,
            markdownDestination(markdown, url.from, url.to),
          );
        }
        return;
      }

      if (node.name === "HTMLTag" || node.name === "HTMLBlock") {
        htmlRanges.push({ from: node.from, to: node.to });
      }
    },
  });

  for (const label of referenceLabels) {
    const definition = referenceDefinitions.get(label);
    if (definition) destinations.push(definition);
  }

  for (const range of htmlRanges) {
    const source = markdown.slice(range.from, range.to);
    for (const match of source.matchAll(htmlImagePattern)) {
      if (match.index === undefined) continue;
      const from = range.from + match.index + match[1].length;
      const raw = match[2];
      destinations.push({
        raw,
        destination: raw,
        from,
        to: from + raw.length,
        syntax: "html",
      });
    }
  }

  destinations.sort((left, right) => left.from - right.from || left.to - right.to);
  return destinations.filter((destination, index) => {
    const previous = destinations[index - 1];
    return !previous
      || previous.from !== destination.from
      || previous.to !== destination.to;
  });
}

function markdownDestination(
  markdown: string,
  from: number,
  to: number,
): ImageDestination {
  const raw = markdown.slice(from, to);
  const angleWrapped = raw.startsWith("<") && raw.endsWith(">");
  return {
    raw,
    destination: angleWrapped ? raw.slice(1, -1) : raw,
    from,
    to,
    syntax: "markdown",
  };
}

function stripLabelBrackets(value: string): string {
  return value.startsWith("[") && value.endsWith("]")
    ? value.slice(1, -1)
    : value;
}

function normalizeReferenceLabel(value: string): string {
  return decodeHTMLStrict(
    value.replace(
      /\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])/g,
      "$1",
    ),
  )
    .trim()
    .replace(/\s+/g, " ")
    .toLocaleLowerCase();
}
