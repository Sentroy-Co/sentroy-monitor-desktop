import { test } from "node:test"
import assert from "node:assert/strict"

import { LANGS, STRINGS, pickLang, translator } from "./i18n.js"
import { formatClock, groupTasks } from "./logic.js"

test("every language has exactly the English keys", () => {
  const keys = Object.keys(STRINGS.en).sort()
  for (const lang of LANGS) {
    assert.deepEqual(Object.keys(STRINGS[lang]).sort(), keys, lang)
    for (const [k, v] of Object.entries(STRINGS[lang])) assert.ok(v.trim(), `${lang}.${k} is empty`)
  }
})

test("placeholders survive translation", () => {
  for (const lang of LANGS) {
    const t = translator(lang)
    assert.match(t("queued", { n: 7 }), /7/)
    assert.match(t("synced", { time: "09:41" }), /09:41/)
  }
})

test("pickLang falls back to English", () => {
  assert.equal(pickLang(["tr-TR", "en"]), "tr")
  assert.equal(pickLang(["de-DE", "zh-CN"]), "zh")
  assert.equal(pickLang(["fr"]), "en")
  assert.equal(pickLang(undefined), "en")
})

test("formatClock", () => {
  assert.equal(formatClock(0), "00:00:00")
  assert.equal(formatClock(3725), "01:02:05")
  assert.equal(formatClock(100 * 3600), "100:00:00")
})

test("groupTasks filters by project and puts mine first", () => {
  const tasks = [
    { id: "a", projectId: "p1", title: "A", mine: false },
    { id: "b", projectId: "p1", title: "B", mine: true },
    { id: "c", projectId: "p2", title: "C", mine: true },
  ]
  assert.deepEqual(groupTasks(tasks, "p1"), { mine: [tasks[1]], others: [tasks[0]] })
  assert.deepEqual(groupTasks(tasks, null).mine.map((t) => t.id), ["b", "c"])
})
