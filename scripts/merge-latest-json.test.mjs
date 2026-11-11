import { test } from "node:test"
import assert from "node:assert/strict"

import { mergeLatest } from "./merge-latest-json.mjs"

test("merges platforms from both builds", () => {
  const ci = { version: "0.2.0", notes: "n", pub_date: "x", platforms: { "windows-x86_64": { url: "w", signature: "s" }, "linux-x86_64": { url: "l", signature: "s" } } }
  const mac = { version: "0.2.0", platforms: { "darwin-aarch64": { url: "m", signature: "s" }, "darwin-x86_64": { url: "m", signature: "s" } } }
  const merged = mergeLatest(ci, mac)
  assert.deepEqual(Object.keys(merged.platforms).sort(), ["darwin-aarch64", "darwin-x86_64", "linux-x86_64", "windows-x86_64"])
  assert.equal(merged.notes, "n")
  assert.deepEqual(Object.keys(mergeLatest(null, mac).platforms), ["darwin-aarch64", "darwin-x86_64"])
})

test("refuses to merge different versions", () => {
  assert.throws(() => mergeLatest({ version: "0.1.0", platforms: {} }, { version: "v0.2.0", platforms: {} }))
  assert.doesNotThrow(() => mergeLatest({ version: "v0.2.0", platforms: {} }, { version: "0.2.0", platforms: {} }))
})
