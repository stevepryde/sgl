import { readFile, writeFile } from "node:fs/promises";

const OUTPUTS = [
  ["THIRD_PARTY_NOTICES.txt", ""],
  ["crates/sgl-3d/THIRD_PARTY_NOTICES.txt", "crates/sgl-3d/"],
  ["crates/sgl-post-fx/THIRD_PARTY_NOTICES.txt", "crates/sgl-post-fx/"],
] as const;

// Source locations and provenance are shared with the explicit refresh command.
const upstreams: { component: string; path: string }[] = JSON.parse(
  await readFile("licenses/upstreams.json", "utf8"),
);
const bundled = new Map<string, string[]>();
for (const { component, path } of upstreams) {
  const paths = bundled.get(component) ?? [];
  paths.push(path);
  bundled.set(component, paths);
}

async function generateNotices(prefix: string): Promise<string> {
  const sections = [
    "THIRD-PARTY NOTICES FOR STEVE'S GAME LIBRARY",
    `This file covers third-party code and assets copied, ported or bundled in this
repository. Source headers and adjacent provenance records retain file-level
attribution. Dependencies fetched separately by Cargo are not included here;
their licences still apply when redistributing them, including in compiled games.
Game distributors must include the notices required by the code and assets they
ship. This file does not relicense game-owned code or assets.

Regenerate with: bun scripts/license-notices.ts generate`,
  ];
  for (const [component, paths] of bundled) {
    // Share identical licence bodies, retaining every source path. Distinct
    // copyright or provenance text always keeps its own entry.
    const texts = new Map<string, string[]>();
    for (const path of paths.filter((path) => path.startsWith(prefix))) {
      const text = (await readFile(path, "utf8"))
        .replace(/\r\n?/g, "\n")
        .split("\n")
        .map((line) => line.trimEnd())
        .join("\n")
        .trimEnd();
      const sources = texts.get(text) ?? [];
      sources.push(path);
      texts.set(text, sources);
    }
    for (const [text, sources] of texts) {
      sections.push(`--- ${component}\nSources:\n${sources.join("\n")}\n\n${text}`);
    }
  }
  return `${sections.join("\n\n")}\n`;
}

const [task, ...extra] = Bun.argv.slice(2);
if ((task !== "generate" && task !== "check") || extra.length !== 0) {
  throw new Error("usage: bun scripts/license-notices.ts <generate|check>");
}
for (const [path, prefix] of OUTPUTS) {
  const notices = await generateNotices(prefix);
  if (task === "generate") {
    await writeFile(path, notices, "utf8");
  } else if (await readFile(path, "utf8") !== notices) {
    throw new Error(`${path} is stale; run bun scripts/license-notices.ts generate`);
  }
}
