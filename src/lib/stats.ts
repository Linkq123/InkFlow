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
  let inFence = false;
  content.split("\n").forEach((line, index) => {
    if (/^\s*(```|~~~)/.test(line)) inFence = !inFence;
    if (inFence) return;
    const match = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (match) result.push({ level: match[1].length, text: match[2], line: index + 1 });
  });
  return result;
}
