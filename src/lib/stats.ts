import { markdownLanguage } from "@codemirror/lang-markdown";

export interface DocumentStats {
  words: number;
  lines: number;
  characters: number;
}

export function documentStats(content: string): DocumentStats {
  const chinese = content.match(/[\u3400-\u9fff]/g)?.length ?? 0;
  const latin = content
    .replace(/[\u3400-\u9fff]/g, " ")
    .match(/[\p{L}\p{N}]+(?:['’_-][\p{L}\p{N}]+)*/gu)?.length ?? 0;
  return {
    words: chinese + latin,
    lines: content.length === 0 ? 1 : content.split("\n").length,
    characters: Array.from(content).length,
  };
}

export interface OutlineItem {
  level: number;
  text: string;
  line: number;
}

export function extractOutline(content: string): OutlineItem[] {
  const result: OutlineItem[] = [];
  const newlineOffsets: number[] = [];
  for (let offset = content.indexOf("\n"); offset >= 0; offset = content.indexOf("\n", offset + 1)) {
    newlineOffsets.push(offset);
  }
  const lineAt = (offset: number) => {
    let low = 0;
    let high = newlineOffsets.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (newlineOffsets[middle] < offset) low = middle + 1;
      else high = middle;
    }
    return low + 1;
  };

  markdownLanguage.parser.parse(content).iterate({
    enter(node) {
      const match = /^(?:ATXHeading([1-6])|SetextHeading([12]))$/.exec(node.name);
      if (!match) return;

      let firstMark: { from: number; to: number } | null = null;
      let lastMark: { from: number; to: number } | null = null;
      for (let child = node.node.firstChild; child; child = child.nextSibling) {
        if (child.name === "HeaderMark") {
          firstMark ??= child;
          lastMark = child;
        }
      }

      const isAtx = match[1] !== undefined;
      const textStart = isAtx ? (firstMark?.to ?? node.from) : node.from;
      const textEnd = lastMark && (!isAtx || lastMark.from !== firstMark?.from)
        ? lastMark.from
        : node.to;
      result.push({
        level: Number(match[1] ?? match[2]),
        text: content.slice(textStart, textEnd).trim().replace(/\r?\n[ \t]*/g, " "),
        line: lineAt(node.from),
      });
    },
  });
  return result;
}
