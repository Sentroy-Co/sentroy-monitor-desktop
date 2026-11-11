#!/usr/bin/env node
// Merges updater manifests (latest.json) so the macOS build and the
// Windows/Linux CI build never drop each other's platforms.
//
//   node scripts/merge-latest-json.mjs <base.json> <extra.json> [-o out.json]
//
// Platforms from <extra> replace the same keys in <base>; all others are kept.
// Both files must describe the same version. A missing <base> is treated as empty.
import { existsSync, readFileSync, writeFileSync } from "node:fs"

export function mergeLatest(base, extra) {
  if (base?.version && extra.version && base.version.replace(/^v/, "") !== extra.version.replace(/^v/, "")) {
    throw new Error(`version mismatch: ${base.version} vs ${extra.version}`)
  }
  return {
    version: extra.version ?? base?.version,
    notes: extra.notes || base?.notes || "",
    pub_date: extra.pub_date ?? base?.pub_date ?? new Date().toISOString(),
    platforms: { ...(base?.platforms ?? {}), ...(extra.platforms ?? {}) },
  }
}

const isMain = import.meta.url === `file://${process.argv[1]}`
if (isMain) {
  const args = process.argv.slice(2)
  const o = args.indexOf("-o")
  const out = o >= 0 ? args.splice(o, 2)[1] : null
  const [basePath, extraPath] = args
  if (!basePath || !extraPath) {
    console.error("usage: merge-latest-json.mjs <base.json> <extra.json> [-o out.json]")
    process.exit(2)
  }
  const read = (p) => JSON.parse(readFileSync(p, "utf8"))
  const merged = mergeLatest(existsSync(basePath) ? read(basePath) : null, read(extraPath))
  const json = JSON.stringify(merged, null, 2) + "\n"
  if (out) writeFileSync(out, json)
  else process.stdout.write(json)
}
