/// Parse the Markdown subset our release notes actually use into structured
/// blocks.
///
/// Deliberately returns data, not HTML. The notes come off the network (the
/// updater manifest's `notes`, which the release workflow fills from
/// CHANGELOG.md), and rendering remote text as HTML would be an injection
/// path. Blocks are rendered with ordinary Svelte markup, so every character
/// is escaped by the template.
///
/// Anything outside the subset degrades to plain text rather than being
/// dropped — a reader should never silently lose a line of the notes.

export interface NoteSpan {
  text: string;
  bold?: boolean;
  code?: boolean;
  href?: string;
}

export type NoteBlock =
  | { kind: "heading"; level: number; spans: NoteSpan[] }
  | { kind: "paragraph"; spans: NoteSpan[] }
  | { kind: "item"; spans: NoteSpan[] };

/// The release workflow appends install/first-run boilerplate (Gatekeeper,
/// SmartScreen, chmod) after a `---` rule. It is useful on the releases page
/// and pointless inside an app the reader has already launched.
export function stripTrailingBoilerplate(markdown: string): string {
  const lines = markdown.split("\n");
  const rule = lines.findIndex((line) => /^\s*---\s*$/.test(line));
  return rule === -1 ? markdown : lines.slice(0, rule).join("\n");
}

function pushSpan(spans: NoteSpan[], span: NoteSpan): void {
  if (span.text === "") return;
  spans.push(span);
}

/// Inline: `**bold**`, `` `code` ``, `[text](href)`. Unmatched markers stay as
/// literal text.
function parseInline(text: string): NoteSpan[] {
  const spans: NoteSpan[] = [];
  let plain = "";
  let i = 0;

  const flush = () => {
    pushSpan(spans, { text: plain });
    plain = "";
  };

  while (i < text.length) {
    if (text.startsWith("**", i)) {
      const end = text.indexOf("**", i + 2);
      if (end !== -1) {
        flush();
        pushSpan(spans, { text: text.slice(i + 2, end), bold: true });
        i = end + 2;
        continue;
      }
    }
    if (text[i] === "`") {
      const end = text.indexOf("`", i + 1);
      if (end !== -1) {
        flush();
        pushSpan(spans, { text: text.slice(i + 1, end), code: true });
        i = end + 1;
        continue;
      }
    }
    if (text[i] === "[") {
      const close = text.indexOf("]", i + 1);
      if (close !== -1 && text[close + 1] === "(") {
        const paren = text.indexOf(")", close + 2);
        if (paren !== -1) {
          const href = text.slice(close + 2, paren).trim();
          // Only http(s). A `javascript:` or `data:` href in remote text has
          // no legitimate use here.
          if (/^https?:\/\//i.test(href)) {
            flush();
            pushSpan(spans, { text: text.slice(i + 1, close), href });
            i = paren + 1;
            continue;
          }
        }
      }
    }
    plain += text[i];
    i += 1;
  }
  flush();
  return spans;
}

export function parseReleaseNotes(markdown: string): NoteBlock[] {
  const blocks: NoteBlock[] = [];
  let paragraph: string[] = [];

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    blocks.push({ kind: "paragraph", spans: parseInline(paragraph.join(" ")) });
    paragraph = [];
  };

  for (const raw of stripTrailingBoilerplate(markdown).split("\n")) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (trimmed === "") {
      flushParagraph();
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushParagraph();
      blocks.push({
        kind: "heading",
        level: heading[1].length,
        spans: parseInline(heading[2]),
      });
      continue;
    }

    const item = /^[-*]\s+(.*)$/.exec(trimmed);
    if (item) {
      flushParagraph();
      blocks.push({ kind: "item", spans: parseInline(item[1]) });
      continue;
    }

    // An indented line under a list item is that item wrapping, not a new
    // paragraph — CHANGELOG.md wraps at ~80 columns throughout.
    const last = blocks[blocks.length - 1];
    if (paragraph.length === 0 && /^\s+/.test(line) && last?.kind === "item") {
      last.spans = parseInline(
        last.spans.map(spanText).join("") + " " + trimmed,
      );
      continue;
    }

    paragraph.push(trimmed);
  }
  flushParagraph();

  return blocks;
}

/// Round-trip helper for re-parsing a wrapped item. Loses bold/link markers,
/// so the re-parse re-derives them from the rebuilt source below.
function spanText(span: NoteSpan): string {
  if (span.bold) return `**${span.text}**`;
  if (span.code) return `\`${span.text}\``;
  if (span.href) return `[${span.text}](${span.href})`;
  return span.text;
}
