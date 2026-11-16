// Small pure helpers used by app.js (kept separate so they can be unit-tested).

export function formatClock(seconds) {
  const s = Math.max(0, Math.floor(seconds))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`
}

/** Hours and minutes, e.g. "0:07", "2:45" (for today's totals). */
export function formatHM(seconds) {
  const m = Math.floor(Math.max(0, seconds) / 60)
  return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}`
}

/** Colour band for an activity percentage. */
export function activityBand(pct) {
  if (pct == null) return "none"
  return pct >= 60 ? "high" : pct >= 30 ? "mid" : "low"
}

/** True while a required permission is missing (the sheet must block). */
export function permissionsBlock(items) {
  return (items ?? []).some((p) => p.required && !p.granted)
}

/** Today's per-app rows with each app's share of the largest entry (0..1). */
export function todayRows(apps) {
  const list = (apps ?? []).filter((x) => x.seconds >= 1)
  const max = Math.max(1, ...list.map((x) => x.seconds))
  return list.map((x) => ({ ...x, share: x.seconds / max }))
}

/**
 * Splits a typed or pasted server address into its scheme ("https" | "http")
 * and the rest: trimmed, without the scheme and without trailing slashes.
 * `fallback` is the scheme when the text has none (the toggle's state).
 */
export function splitServerUrl(raw, fallback = "https") {
  let rest = String(raw ?? "").trim()
  let scheme = fallback === "http" ? "http" : "https"
  const m = /^(https?):\/\//i.exec(rest)
  if (m) {
    scheme = m[1].toLowerCase()
    rest = rest.slice(m[0].length).trim()
  }
  return { scheme, rest: rest.replace(/\/+$/, "") }
}

/** `<scheme>://<rest>` for `set_server`, or "" when there is no host yet. */
export function buildServerUrl(scheme, rest) {
  const p = splitServerUrl(rest, scheme)
  return p.rest ? `${p.scheme}://${p.rest}` : ""
}
