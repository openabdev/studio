import { defineConfig } from "vite";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";

// Build stamp — version (package.json), short git sha, build time — injected at
// build so every artifact self-identifies which commit it came from (the app
// `version` alone is static across builds and can't tell them apart).
function gitSha(): string {
  try {
    return execSync("git rev-parse --short HEAD", {
      stdio: ["ignore", "pipe", "ignore"],
    })
      .toString()
      .trim();
  } catch {
    return process.env.GITHUB_SHA?.slice(0, 7) ?? "unknown";
  }
}

const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8"),
) as { version?: string };

// `base: "./"` so the built `dist/index.html` opens standalone over file://
// (the browser-console / preview use case) with no dev server.
export default defineConfig({
  base: "./",
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version ?? "0.0.0"),
    __BUILD_SHA__: JSON.stringify(gitSha()),
    __BUILD_TIME__: JSON.stringify(
      new Date().toISOString().slice(0, 16).replace("T", " ") + "Z",
    ),
  },
  build: { outDir: "dist", emptyOutDir: true },
});
