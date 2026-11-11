import { test } from "node:test"
import assert from "node:assert/strict"

import { LANGS, STRINGS, pickLang, translator } from "./i18n.js"
import { activityBand, formatClock, formatHM, groupTasks, permissionsBlock, todayRows } from "./logic.js"

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
    assert.match(t("autoStopped", { n: 45 }), /45/)
    assert.match(t("updateAvailable", { version: "9.8.7" }), /9\.8\.7/)
    assert.match(t("shotsPolicy", { n: 2 }), /2/)
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

test("formatHM and activityBand", () => {
  assert.equal(formatHM(0), "0:00")
  assert.equal(formatHM(7 * 60 + 59), "0:07")
  assert.equal(formatHM(2 * 3600 + 45 * 60), "2:45")
  assert.equal(activityBand(null), "none")
  assert.equal(activityBand(75), "high")
  assert.equal(activityBand(45), "mid")
  assert.equal(activityBand(5), "low")
})

test("permissionsBlock only for required, missing items", () => {
  assert.equal(permissionsBlock([]), false)
  assert.equal(permissionsBlock([{ kind: "notifications", required: false, granted: false }]), false)
  assert.equal(permissionsBlock([{ kind: "screen", required: true, granted: true }]), false)
  assert.equal(permissionsBlock([{ kind: "screen", required: true, granted: false }]), true)
})

test("todayRows computes shares and skips empty entries", () => {
  const rows = todayRows([
    { taskId: "a", seconds: 600 },
    { taskId: "b", seconds: 300 },
    { taskId: "c", seconds: 0 },
  ])
  assert.deepEqual(rows.map((r) => [r.taskId, r.share]), [["a", 1], ["b", 0.5]])
  assert.deepEqual(todayRows(undefined), [])
})
