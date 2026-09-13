import { describe, it, expect } from "vitest";
import {
  mdToHtml,
  transcriptHtml,
  turnHtml,
  appendUser,
  settlePending,
  appendChunk,
  appendImage,
  endTurn,
  isRenderableImage,
  type ChatTurn,
  type ChatImage,
} from "./chat";

const PNG_PIXEL: ChatImage = {
  data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
  mimeType: "image/png",
};

function agent(partial: Partial<ChatTurn>): ChatTurn {
  return { id: 1, role: "agent", text: "", streaming: false, ...partial };
}

describe("mdToHtml", () => {
  it("renders basic markdown", () => {
    const html = mdToHtml("**bold** and `code`");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
  });

  it("escapes raw HTML in agent output (html: false)", () => {
    const html = mdToHtml("<img src=x onerror=alert(1)>");
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
  });

  it("does not emit a javascript: anchor (markdown-it validateLink blocks it)", () => {
    const html = mdToHtml("[click](javascript:alert(1))");
    expect(html).not.toContain("<a ");
    expect(html).not.toContain('href="javascript:');
  });
});

describe("transcript reducers", () => {
  it("appendUser adds a non-streaming user turn", () => {
    const turns = appendUser([], 1, "hello");
    expect(turns).toHaveLength(1);
    expect(turns[0]).toMatchObject({ role: "user", text: "hello", streaming: false });
    expect(turns[0].pending).toBe(false);
  });

  it("appendUser can mark a turn pending; settlePending clears it by id", () => {
    let turns = appendUser([], 7, "queued one", true);
    expect(turns[0].pending).toBe(true);
    turns = settlePending(turns, 7);
    expect(turns[0].pending).toBe(false);
  });

  it("turnHtml renders a pending user turn with the queued marker", () => {
    const html = turnHtml(
      { id: 1, role: "user", text: "hi", streaming: false, pending: true },
      mdToHtml,
    );
    expect(html).toContain("chat-pending");
    expect(html).toContain("queued");
    // a settled user turn has neither
    const sent = turnHtml(
      { id: 1, role: "user", text: "hi", streaming: false },
      mdToHtml,
    );
    expect(sent).not.toContain("chat-pending");
    expect(sent).not.toContain("chat-queued");
  });

  it("appendChunk opens an agent turn on the first chunk, appends after", () => {
    let turns = appendUser([], 1, "hi");
    turns = appendChunk(turns, 2, "he");
    expect(turns).toHaveLength(2);
    expect(turns[1]).toMatchObject({ role: "agent", text: "he", streaming: true, id: 2 });
    turns = appendChunk(turns, 99, "llo");
    // still one agent turn, id preserved from the opening chunk
    expect(turns).toHaveLength(2);
    expect(turns[1]).toMatchObject({ text: "hello", id: 2 });
  });

  it("endTurn finalizes the open agent turn with its stop reason", () => {
    let turns = appendChunk([], 1, "done");
    turns = endTurn(turns, 2, "end_turn");
    expect(turns[0]).toMatchObject({ streaming: false, stopReason: "end_turn", text: "done" });
  });

  it("endTurn with no open turn synthesizes an empty finalized turn", () => {
    const turns = endTurn([], 1, "cancelled");
    expect(turns).toHaveLength(1);
    expect(turns[0]).toMatchObject({ role: "agent", text: "", streaming: false, stopReason: "cancelled" });
  });

  it("reducers do not mutate the input array", () => {
    const start: ChatTurn[] = [];
    appendUser(start, 1, "x");
    expect(start).toHaveLength(0);
  });

  it("appendImage opens an agent turn on the first image, appends after", () => {
    let turns = appendUser([], 1, "hi");
    turns = appendImage(turns, 2, PNG_PIXEL);
    expect(turns).toHaveLength(2);
    expect(turns[1]).toMatchObject({ role: "agent", streaming: true, id: 2, images: [PNG_PIXEL] });
    const second: ChatImage = { ...PNG_PIXEL, mimeType: "image/jpeg" };
    turns = appendImage(turns, 99, second);
    // still one agent turn, id preserved from the opening image
    expect(turns).toHaveLength(2);
    expect(turns[1].images).toEqual([PNG_PIXEL, second]);
  });

  it("appendUser attaches images to the user's own turn", () => {
    const turns = appendUser([], 1, "look", false, [PNG_PIXEL]);
    expect(turns[0].images).toEqual([PNG_PIXEL]);
  });
});

describe("isRenderableImage", () => {
  it("accepts an allow-listed mime type with base64-shaped data", () => {
    expect(isRenderableImage(PNG_PIXEL)).toBe(true);
  });

  it("rejects a mime type outside the allow-list", () => {
    expect(isRenderableImage({ ...PNG_PIXEL, mimeType: "image/svg+xml" })).toBe(false);
  });

  it("rejects data that isn't valid base64 shape (defends the src attribute)", () => {
    expect(isRenderableImage({ ...PNG_PIXEL, data: '"><script>alert(1)</script>' })).toBe(false);
  });

  it("rejects empty data", () => {
    expect(isRenderableImage({ ...PNG_PIXEL, data: "" })).toBe(false);
  });
});

describe("turnHtml / transcriptHtml", () => {
  it("renders the user's own prompt as markdown", () => {
    const html = turnHtml(
      { id: 1, role: "user", text: "**hi** and `x`", streaming: false },
      mdToHtml,
    );
    expect(html).toContain("chat-user");
    expect(html).toContain("chat-md");
    expect(html).toContain("<strong>hi</strong>");
    expect(html).toContain("<code>x</code>");
  });

  it("still escapes raw HTML in user text (markdown-it html:false)", () => {
    const html = turnHtml(
      { id: 1, role: "user", text: "<b>hi</b> & bye", streaming: false },
      mdToHtml,
    );
    expect(html).toContain("&lt;b&gt;hi&lt;/b&gt; &amp; bye");
    expect(html).not.toContain("<b>hi</b>");
  });

  it("shows a spinner while streaming and no copy button", () => {
    const html = turnHtml(agent({ text: "partial", streaming: true }), mdToHtml);
    expect(html).toContain("chat-spinner");
    expect(html).not.toContain("chat-copy");
  });

  it("renders markdown and a copy button once final", () => {
    const html = turnHtml(agent({ id: 7, text: "**hi**", streaming: false }), mdToHtml);
    expect(html).toContain("<strong>hi</strong>");
    expect(html).toContain('data-copy="7"');
    expect(html).not.toContain("chat-spinner");
  });

  it("shows a non-ordinary stop reason but hides end_turn", () => {
    expect(turnHtml(agent({ stopReason: "cancelled" }), mdToHtml)).toContain("cancelled");
    expect(turnHtml(agent({ stopReason: "end_turn" }), mdToHtml)).not.toContain("chat-reason");
  });

  it("uses the injected agent renderer (sanitizer hook)", () => {
    const html = transcriptHtml([agent({ text: "x", streaming: false })], () => "SANITIZED");
    expect(html).toContain("SANITIZED");
  });

  it("renders an empty-state message for no turns", () => {
    expect(transcriptHtml([])).toContain("chat-empty");
  });

  it("renders a valid image as a data-URI <img>", () => {
    const html = turnHtml(agent({ text: "look", streaming: false, images: [PNG_PIXEL] }), mdToHtml);
    expect(html).toContain("chat-images");
    expect(html).toContain(`data:${PNG_PIXEL.mimeType};base64,${PNG_PIXEL.data}`);
  });

  it("drops an unrenderable image instead of emitting it", () => {
    const bad: ChatImage = { data: "not base64!!", mimeType: "image/png" };
    const html = turnHtml(agent({ text: "look", streaming: false, images: [bad] }), mdToHtml);
    expect(html).not.toContain("chat-images");
    expect(html).not.toContain(bad.data);
  });

  it("renders images on a streaming turn too", () => {
    const html = turnHtml(agent({ text: "", streaming: true, images: [PNG_PIXEL] }), mdToHtml);
    expect(html).toContain("chat-images");
  });

  it("renders the operator's own attached image on a user turn", () => {
    const html = turnHtml(
      { id: 1, role: "user", text: "look", streaming: false, images: [PNG_PIXEL] },
      mdToHtml,
    );
    expect(html).toContain("chat-images");
  });
});
