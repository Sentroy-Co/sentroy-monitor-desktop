import { test } from "node:test"
import assert from "node:assert/strict"

import { LANGS, STRINGS, pickLang, translator } from "./i18n.js"
import { activityBand, buildServerUrl, formatClock, formatHM, permissionsBlock, splitServerUrl, todayRows } from "./logic.js"

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
    assert.match(t("eodSummary", { time: "6:42" }), /6:42/)
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
    { name: "Figma", seconds: 600 },
    { name: "Safari", seconds: 300 },
    { name: "Mail", seconds: 0 },
  ])
  assert.deepEqual(rows.map((r) => [r.name, r.share]), [["Figma", 1], ["Safari", 0.5]])
  assert.deepEqual(todayRows(undefined), [])
})

test("splitServerUrl moves a typed scheme into the toggle", () => {
  assert.deepEqual(splitServerUrl("  HTTPS://monitor.acme.io/// "), { scheme: "https", rest: "monitor.acme.io" })
  assert.deepEqual(splitServerUrl("http://localhost:3000/", "https"), { scheme: "http", rest: "localhost:3000" })
  assert.deepEqual(splitServerUrl("monitor.acme.io/sub/", "http"), { scheme: "http", rest: "monitor.acme.io/sub" })
  assert.deepEqual(splitServerUrl("", "nonsense"), { scheme: "https", rest: "" })
})

test("buildServerUrl normalises before set_server", () => {
  assert.equal(buildServerUrl("https", " monitor.acme.io/ "), "https://monitor.acme.io")
  assert.equal(buildServerUrl("https", "http://localhost:3000"), "http://localhost:3000")
  assert.equal(buildServerUrl("http", "127.0.0.1:8080//"), "http://127.0.0.1:8080")
  assert.equal(buildServerUrl("https", "   "), "")
  assert.equal(buildServerUrl("https", "https://"), "")
})
