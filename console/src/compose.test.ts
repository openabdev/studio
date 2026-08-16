import { describe, it, expect } from "vitest";
import { renderPreviewHtml, libraryNames, type BundlePreview, type Library } from "./compose";

const PREVIEW: BundlePreview = {
  image_tag: "ghcr.io/openabdev/openab:0.9.0-claude",
  digest: "sha256:abc123",
  files: [
    { path: ".claude/skills/memory/SKILL.md", text: "# memory\n", bytes: 9, binary: false },
    { path: "CLAUDE.md", text: "# persona\n", bytes: 10, binary: false },
  ],
};

describe("renderPreviewHtml", () => {
  it("shows the image tag, digest and file count", () => {
    const html = renderPreviewHtml(PREVIEW);
    expect(html).toContain("ghcr.io/openabdev/openab:0.9.0-claude");
    expect(html).toContain("sha256:abc123");
    // file count
    expect(html).toContain("files</span> 2");
  });

  it("lists each file path with its content", () => {
    const html = renderPreviewHtml(PREVIEW);
    expect(html).toContain(".claude/skills/memory/SKILL.md");
    expect(html).toContain("CLAUDE.md");
    expect(html).toContain("# persona\n");
  });

  it("escapes HTML in paths and content (no injection)", () => {
    const html = renderPreviewHtml({
      image_tag: "img",
      digest: "sha256:x",
      files: [{ path: "<evil>.md", text: "<script>alert(1)</script>", bytes: 5, binary: false }],
    });
    expect(html).not.toContain("<script>alert(1)</script>");
    expect(html).toContain("&lt;script&gt;");
    expect(html).toContain("&lt;evil&gt;.md");
  });

  it("does not dump content for a binary file", () => {
    const html = renderPreviewHtml({
      image_tag: "img",
      digest: "sha256:x",
      files: [{ path: "blob", text: "�", bytes: 3, binary: true }],
    });
    expect(html).toContain("binary");
    expect(html).toContain("not shown");
  });
});

describe("libraryNames", () => {
  it("returns sorted template and overlay names", () => {
    const lib: Library = {
      templates: { zeta: {} as never, alpha: {} as never },
      overlays: { orca: {} as never },
      skills: { skills: {} },
    };
    expect(libraryNames(lib)).toEqual({ templates: ["alpha", "zeta"], overlays: ["orca"] });
  });

  it("tolerates missing maps", () => {
    const lib = { skills: { skills: {} } } as unknown as Library;
    expect(libraryNames(lib)).toEqual({ templates: [], overlays: [] });
  });
});
