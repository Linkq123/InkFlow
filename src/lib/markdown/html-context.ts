type Namespace = "html" | "svg" | "math";
interface Element {
  name: string;
  namespace: Namespace;
  integration: "html" | "math" | null;
}

const voidElements = new Set("area base br col embed hr img input link meta param source track wbr".split(" "));
const foreignBreakouts = new Set(
  "b big blockquote body br center code dd div dl dt em embed h1 h2 h3 h4 h5 h6 head hr i img li listing menu meta nobr ol p pre ruby s small span strong strike sub sup table tt u ul var".split(" "),
);

// Only retain ancestry inside foreign content. Keep these transitions aligned
// with HtmlNamespaceContext in asset.rs, including HTML integration points.
export class HtmlNamespaceContext {
  private elements: Element[] = [];
  private rawElement: string | undefined;

  get active(): boolean { return this.elements.length > 0; }
  get foreign(): boolean { return this.elements.at(-1)?.namespace !== "html" && this.active; }

  start(name: string, selfClosing = false, encoding?: string, fontAttributes = false): boolean {
    const parent = this.elements.at(-1);
    let namespace = parent?.namespace ?? "html";
    if (parent?.integration === "html"
      || (parent?.integration === "math" && !["mglyph", "malignmark"].includes(name))
      || (parent?.namespace === "math" && parent.name === "annotation-xml" && name === "svg")) {
      namespace = "html";
    }
    if (namespace !== "html" && (foreignBreakouts.has(name) || (name === "font" && fontAttributes))) {
      while (this.elements.length) {
        const current = this.elements.at(-1)!;
        if (current.namespace === "html" || current.integration) break;
        this.elements.pop();
      }
      namespace = "html";
    }
    if (namespace === "html") {
      if (name === "svg") namespace = "svg";
      else if (name === "math") namespace = "math";
    }
    const integration = namespace === "svg" && ["title", "desc", "foreignobject"].includes(name)
      ? "html"
      : namespace === "math" && name === "annotation-xml"
        && ["text/html", "application/xhtml+xml"].includes(encoding?.toLowerCase() ?? "")
        ? "html"
        : namespace === "math" && ["mi", "mo", "mn", "ms", "mtext"].includes(name)
          ? "math" : null;
    if ((namespace !== "html" || this.active)
      && !(namespace === "html" ? voidElements.has(name) : selfClosing)) {
      this.elements.push({ name, namespace, integration });
    }
    return namespace === "html";
  }

  end(name: string): void {
    if (this.foreign && (name === "p" || name === "br")) {
      while (this.elements.length) {
        const current = this.elements.at(-1)!;
        if (current.namespace === "html" || current.integration) break;
        this.elements.pop();
      }
    }
    for (let index = this.elements.length - 1; index >= 0; index--) {
      if (this.elements[index].name === name) {
        this.elements.length = index;
        break;
      }
    }
    if (this.rawElement === name) this.rawElement = undefined;
  }

  enterRaw(name: string): void { this.rawElement = name; }

  generated(name: string, closing: boolean): void {
    // Generated tags are ignored by the tree builder inside raw text until
    // its first non-void closing tag. Plaintext never leaves that state.
    if (this.rawElement) {
      if (closing && this.rawElement !== "plaintext") this.end(this.rawElement);
    } else if (closing) this.end(name);
    else this.start(name);
  }
}
