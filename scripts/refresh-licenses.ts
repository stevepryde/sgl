import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

type Upstream = { path: string; url: string; prefix?: string };
type Supplement = { source: string; text: string; start?: string; end?: string; manual?: boolean };
const root = resolve(import.meta.dir, "..");
const upstreams: Upstream[] = JSON.parse(await readFile(resolve(root, "licenses/upstreams.json"), "utf8"));
const supplements: Supplement[] = JSON.parse(await readFile(resolve(root, "licenses/supplemental-notices.json"), "utf8"));
if (Bun.argv.length > 2) throw new Error("usage: bun scripts/refresh-licenses.ts");

const downloads = new Map<string, Promise<string>>();
function download(url: string): Promise<string> {
  let pending = downloads.get(url);
  if (!pending) {
    pending = fetch(url).then(async (response) => {
      if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
      return (await response.text()).replace(/\r\n?/g, "\n");
    });
    downloads.set(url, pending);
  }
  return pending;
}

// Finish every download before writing: a failed upstream request must not leave
// a mixture of refreshed licences and stale generated notices.
const files = await Promise.all(upstreams.map(async (upstream) => ({
  path: resolve(root, upstream.path),
  text: (upstream.prefix ?? "") + await download(upstream.url),
})));
await Promise.all(supplements.map(async (supplement) => {
  if (supplement.manual) return; // Recorded upstream omission; not an invented licence.
  let text = await download(supplement.source);
  if (supplement.start) {
    const start = text.indexOf(supplement.start);
    if (start < 0) throw new Error(`Licence excerpt start not found: ${supplement.source}`);
    text = text.slice(start);
  }
  if (supplement.end) {
    const end = text.indexOf(supplement.end);
    if (end < 0) throw new Error(`Licence excerpt end not found: ${supplement.source}`);
    text = text.slice(0, end);
  }
  supplement.text = text.trimEnd() + "\n";
}));

for (const file of files) {
  const current = await readFile(file.path, "utf8");
  // Preserve original vendor bytes when only line endings/trailing blank lines differ.
  if (current.replace(/\r\n?/g, "\n").trimEnd() !== file.text.trimEnd()) {
    await writeFile(file.path, file.text);
    console.log(`Updated ${file.path}`);
  }
}
await writeFile(resolve(root, "licenses/supplemental-notices.json"), `${JSON.stringify(supplements, null, 2)}\n`);
const result = Bun.spawnSync(["bun", "scripts/license-notices.ts", "generate"], { cwd: root, stdout: "inherit", stderr: "inherit" });
if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
console.log("Refreshed pinned upstream licence texts and SGL source notices. Regenerate distribution notices for any changed dependencies or ports.");
