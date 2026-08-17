import { describe, it, expect } from "vitest";
import {
  mdToHtml,
  transcriptHtml,
  turnHtml,
  appendUser,
  appendChunk,
  endTurn,
  type ChatTurn,
} from "./chat";

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
});
