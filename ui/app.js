import { pickLang, translator } from "./i18n.js"
import { activityBand, buildServerUrl, formatClock, formatHM, permissionsBlock, splitServerUrl, todayRows } from "./logic.js"

const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const lang = pickLang(navigator.languages)
const t = translator(lang)
document.documentElement.lang = lang
if (/Mac/.test(navigator.userAgent)) document.body.classList.add("mac")

const $ = (id) => document.getElementById(id)
for (const el of document.querySelectorAll("[data-t]")) el.textContent = t(el.dataset.t)
for (const el of document.querySelectorAll("[data-t-title]")) el.title = t(el.dataset.tTitle)
for (const el of document.querySelectorAll("[data-t-placeholder]")) el.placeholder = t(el.dataset.tPlaceholder)
for (const el of document.querySelectorAll("[data-t-aria]")) el.setAttribute("aria-label", t(el.dataset.tAria))

let state = null
const timeFmt = new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit" })
const KNOWN_ERRORS = ["offline", "invalidServer", "signOutFirst", "permissionsRequired", "updateFailed"]

function notice(key) {
  if (!key) {
    $("notice").hidden = true
    return
  }
  $("notice-text").textContent = key === "autoStopped" ? t(key, { n: state?.policy?.idleStopMinutes ?? 30 }) : t(key)
  $("notice").hidden = false
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args)
  } catch (e) {
    const key = String(e)
    if (key === "permissionsRequired") {
      openPermissions(true)
    } else if (KNOWN_ERRORS.includes(key)) {
      notice(key === "offline" ? "offlineError" : key)
    } else {
      console.error(e)
    }
    return undefined
  }
}

function el(tag, props = {}, ...children) {
  const n = document.createElement(tag)
  Object.assign(n, props)
  n.append(...children)
  return n
}

let lastBarsKey = ""
function renderBars(bars) {
  const key = JSON.stringify(bars)
  if (key === lastBarsKey) return
  lastBarsKey = key
  $("bars").replaceChildren(
    ...bars.map((b) => {
      const pct = b.percent
      const band = activityBand(pct)
      const fill = el("div", { className: `bar-fill ${band}` })
      fill.style.height = pct == null ? "0" : `${Math.max(6, pct)}%`
      const time = timeFmt.format(new Date(b.start))
      return el(
        "div",
        { className: "bar", title: pct == null ? time : `${time} · ${pct}%` },
        el("div", { className: "bar-track" }, fill),
        el("span", { textContent: pct == null ? "–" : `${pct}%` }),
      )
    }),
  )
}

function renderToday(s) {
  const rows = todayRows(s.todayApps)
  $("today-total").textContent = formatHM(s.todaySeconds)
  $("today-empty").hidden = rows.length > 0
  $("today-list").replaceChildren(
    ...rows.map((r) => {
      const name = el("span", { className: "name", textContent: r.name, title: r.name })
      const share = el("span", { className: "share" }, el("i"))
      share.firstChild.style.width = `${Math.round(r.share * 100)}%`
      return el("li", {}, name, el("span", { className: "dur", textContent: formatHM(r.seconds) }), share)
    }),
  )
}

function render(s) {
  const policyChanged = JSON.stringify(state?.policy) !== JSON.stringify(s.policy) || state?.signedIn !== s.signedIn
  state = s
  if (!$("perm").hidden) renderPolicy(s)
  for (const v of document.querySelectorAll(".version-line")) v.textContent = `Sentroy Monitor v${s.version}`
  const screen = !s.consentAccepted ? "consent" : !s.signedIn ? "signin" : "main"
  for (const id of ["consent", "signin", "main"]) $(id).hidden = id !== screen

  if (screen === "signin") {
    $("signin-idle").hidden = !!s.login
    $("signin-pending").hidden = !s.login
    if (s.login) $("user-code").textContent = s.login.userCode
    syncServer(s)
  }

  if (screen === "main") {
    $("company").textContent = s.companyName ?? "Sentroy Monitor"
    $("user").textContent = s.userName ?? ""
    const toggle = $("toggle")
    toggle.setAttribute("aria-pressed", String(s.tracking))
    toggle.setAttribute("aria-label", s.tracking ? t("stop") : t("start"))
    toggle.title = s.tracking ? t("stop") : t("start")
    $("clock").textContent = formatClock(s.sessionSeconds)
    $("current").textContent = s.tracking ? t("tracking") : t("notTracking")
    $("today").textContent = formatHM(s.todaySeconds)
    const pct = s.activityPercent
    $("activity").textContent = pct == null ? "—" : `${pct}%`
    const bar = $("meter-bar")
    bar.style.width = `${pct ?? 0}%`
    const band = activityBand(pct)
    bar.style.background = band === "mid" ? "var(--warn)" : band === "low" ? "var(--bad)" : ""
    const shots = s.policy?.screenshots
    $("shots").hidden = !shots?.enabled
    if (shots?.enabled) {
      $("shots-text").textContent = t("shotsPolicy", { n: shots.perTenMinutes }) + (shots.blur ? ` · ${t("shotsBlurred")}` : "")
    }
    renderBars(s.bars ?? [])
    renderToday(s)

    $("update").hidden = !s.update
    if (s.update) {
      $("update-text").textContent = s.updating ? t("updating") : t("updateAvailable", { version: s.update.version })
      $("update-install").disabled = s.updating
    }

    $("dot").className = `dot ${s.online ? "on" : "off"}`
    $("online").textContent = s.online ? t("online") : t("offline")
    $("sync").textContent =
      s.queued > 1 ? t("queued", { n: s.queued }) : s.lastSync ? t("synced", { time: timeFmt.format(new Date(s.lastSync)) }) : ""
    $("sep").hidden = !$("sync").textContent
    $("version").textContent = `v${s.version}`
  }
  if (s.notice) notice(s.notice)
  if (screen === "main" && policyChanged) checkPermissions(false)
  if (screen !== "main") closePermissions()
  if (screen !== "main" || !s.tracking) closeStopDialog()
}

// ── sign-in: Sentroy or a self-hosted server ────────────────────────────────

let scheme = "https"
let selfHosted = false
let lastServer = null
// Server changes run one after another (a blur save, then a click), never interleaved.
let serverOp = Promise.resolve()
const serially = (fn) => (serverOp = serverOp.then(fn, fn))

function setScheme(next) {
  scheme = next === "http" ? "http" : "https"
  for (const b of document.querySelectorAll(".scheme button")) b.setAttribute("aria-pressed", String(b.dataset.scheme === scheme))
  $("http-hint").hidden = scheme !== "http"
}

function setSelfHosted(on, focus = false) {
  selfHosted = on
  $("pick-self").setAttribute("aria-expanded", String(on))
  $("server-form").hidden = !on
  $("login-sentroy").classList.toggle("quiet", on)
  if (on && focus) $("server-host").focus()
}

/** Opens "Self-hosted" with the fields filled when the stored server is not Sentroy. */
function syncServer(s) {
  if (s.serverUrl === lastServer) return
  lastServer = s.serverUrl
  const custom = s.serverUrl !== s.defaultServer
  if (custom && document.activeElement !== $("server-host")) {
    const p = splitServerUrl(s.serverUrl)
    setScheme(p.scheme)
    $("server-host").value = p.rest
  }
  setSelfHosted(custom)
}

/** Normalises the field and stores it with `set_server`. False when empty or refused. */
async function saveSelfHosted({ force = false } = {}) {
  const p = splitServerUrl($("server-host").value, scheme)
  setScheme(p.scheme)
  $("server-host").value = p.rest
  const url = buildServerUrl(p.scheme, p.rest)
  if (!url) return false
  if (!force && url === state?.serverUrl) return true
  return (await call("set_server", { url })) !== undefined
}

$("server-host").addEventListener("input", (e) => {
  // A pasted or typed scheme moves into the toggle.
  if (/^\s*https?:\/\//i.test(e.target.value)) {
    const p = splitServerUrl(e.target.value, scheme)
    setScheme(p.scheme)
    e.target.value = p.rest
  }
})
$("server-host").addEventListener("blur", () => {
  if (selfHosted) serially(() => saveSelfHosted())
})
for (const b of document.querySelectorAll(".scheme button")) {
  b.addEventListener("click", () => {
    setScheme(b.dataset.scheme)
    if ($("server-host").value.trim()) serially(() => saveSelfHosted())
  })
}
$("pick-self").addEventListener("click", () => setSelfHosted(!selfHosted, true))
$("login-sentroy").addEventListener("click", () =>
  serially(async () => {
    if ((await call("set_server", { url: state?.defaultServer })) === undefined) return
    await call("start_login")
  }),
)
$("server-form").addEventListener("submit", (e) => {
  e.preventDefault()
  serially(async () => {
    if (!$("server-host").value.trim()) return $("server-host").focus()
    if (await saveSelfHosted({ force: true })) await call("start_login")
  })
})

// ── stop dialog: end of day or a short break ────────────────────────────────

function openStopDialog() {
  if (!state?.tracking) return
  $("main").querySelector(".menu").open = false
  closePermissions()
  $("eod-summary").textContent = t("eodSummary", { time: formatHM(state.todaySeconds) })
  $("eod").hidden = false
  $("eod-note").focus()
}

function closeStopDialog() {
  $("eod").hidden = true
}

async function stopWith(reason) {
  const note = reason === "end_of_day" ? $("eod-note").value : null
  closeStopDialog()
  await call("stop_tracking", { reason, note })
  // The draft survives "Be right back" and cancel; it is sent with the end of the day.
  if (reason === "end_of_day") $("eod-note").value = ""
}

$("eod-form").addEventListener("submit", (e) => {
  e.preventDefault()
  stopWith("end_of_day")
})
$("eod-break").addEventListener("click", () => stopWith("break"))
$("eod-close").addEventListener("click", closeStopDialog)
$("eod").addEventListener("click", (e) => {
  if (e.target === $("eod")) closeStopDialog()
})
$("eod-note").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    $("eod-form").requestSubmit()
  }
})
listen("confirm-stop", () => openStopDialog())

// ── permissions sheet ───────────────────────────────────────────────────────

let permTimer = null
let permItems = []

function renderPermissions(items) {
  permItems = items
  const blocking = permissionsBlock(items)
  $("perm-list").replaceChildren(
    ...items.map((p) => {
      const label = p.kind === "screen" ? t("permScreen") : t("permNotifications")
      const why = p.kind === "screen" ? t("permScreenWhy") : t("permNotificationsWhy")
      const badge = p.granted
        ? el("span", { className: "badge ok", textContent: t("granted") })
        : p.required
          ? el("span", { className: "badge missing", textContent: t("missing") })
          : el("span", { className: "badge optional", textContent: t("recommended") })
      const actions = el("div", { className: "perm-actions" })
      if (!p.granted) {
        const allow = el("button", { className: "primary", textContent: t("allow") })
        allow.addEventListener("click", async () => renderPermissions((await call("request_permission", { kind: p.kind })) ?? permItems))
        actions.append(allow)
        if (p.hasSettings) {
          const open = el("button", { textContent: t("openSettings") })
          open.addEventListener("click", () => call("open_permission_settings", { kind: p.kind }))
          actions.append(open)
        }
      }
      return el(
        "li",
        {},
        el("div", { className: "perm-row" }, el("strong", { textContent: label }), badge),
        el("p", { className: "muted small", textContent: why }),
        ...(actions.childElementCount ? [actions] : []),
      )
    }),
  )
  renderPolicy(state)
  $("perm-restart").hidden = !items.some((p) => p.required && !p.granted && p.needsRestart)
  $("perm-continue").disabled = blocking
}

/** What the company configured on the web, read-only (the server's policy). */
function renderPolicy(s) {
  const p = s?.policy
  if (!p) return
  const shots = p.screenshots ?? {}
  const on = (v) => el("span", { className: v ? "pill on" : "pill", textContent: v ? t("on") : t("off") })
  const sitesSupported = s.platform === "macos"
  const rows = [
    [t("policyScreenshots"), shots.enabled ? el("span", { className: "pill on", textContent: t("policyEvery", { n: shots.perTenMinutes ?? 1 }) }) : on(false)],
    ...(shots.enabled ? [[t("policyBlur"), on(!!shots.blur)]] : []),
    [
      t("policySites"),
      p.sites && !sitesSupported ? el("span", { className: "pill", textContent: t("policySitesUnsupported") }) : on(!!p.sites),
    ],
    [t("policyIdle"), el("span", { className: "pill", textContent: t("policyMinutes", { n: p.idleStopMinutes ?? 30 }) })],
  ]
  $("policy-list").replaceChildren(...rows.flatMap(([label, value]) => [el("dt", { textContent: label }), el("dd", {}, value)]))
}

function closePermissions() {
  $("perm").hidden = true
  clearInterval(permTimer)
  permTimer = null
}

/** Shows the sheet when a required permission is missing (or always, when asked). */
async function checkPermissions(force) {
  const items = await call("permissions_status")
  if (!items) return
  if (!force && !permissionsBlock(items)) return
  openPermissions(false, items)
}

async function openPermissions(_fromStart, items) {
  renderPermissions(items ?? (await call("permissions_status")) ?? [])
  $("perm").hidden = false
  clearInterval(permTimer)
  // Live re-check while the sheet is open (the user grants in System Settings).
  permTimer = setInterval(async () => {
    const next = await call("permissions_status")
    if (next && JSON.stringify(next) !== JSON.stringify(permItems)) renderPermissions(next)
  }, 2000)
}

$("perm-continue").addEventListener("click", () => {
  if (!permissionsBlock(permItems)) closePermissions()
})
$("perm-restart-btn").addEventListener("click", () => call("restart_app"))
$("perm-signout").addEventListener("click", () => {
  closePermissions()
  call("logout")
})
$("permissions").addEventListener("click", () => {
  $("main").querySelector(".menu").open = false
  openPermissions(false)
})

// ── screenshot notice ───────────────────────────────────────────────────────

let toastTimer = null
listen("screenshot", (e) => {
  const text = e.payload?.blurred ? t("screenshotTakenBlurred") : t("screenshotTaken")
  const toast = $("toast")
  $("toast-text").textContent = text
  toast.hidden = true
  void toast.offsetWidth // restart the animation
  toast.hidden = false
  clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (toast.hidden = true), 3300)
  call("notify_screenshot", { title: "Sentroy Monitor", body: text })
})

// ── actions ─────────────────────────────────────────────────────────────────

$("accept").addEventListener("click", () => call("accept_consent"))
$("open-again").addEventListener("click", () => call("open_login_page"))
$("cancel-login").addEventListener("click", () => call("cancel_login"))
$("toggle").addEventListener("click", () => (state?.tracking ? openStopDialog() : call("start_tracking")))
$("update-install").addEventListener("click", () => call("install_update"))
$("dashboard").addEventListener("click", () => {
  $("main").querySelector(".menu").open = false
  call("open_dashboard", { lang })
})
$("refresh").addEventListener("click", () => {
  $("main").querySelector(".menu").open = false
  call("refresh")
})
$("signout").addEventListener("click", () => {
  $("main").querySelector(".menu").open = false
  call("logout")
})
$("notice-close").addEventListener("click", () => {
  notice(null)
  call("dismiss_notice")
})
document.addEventListener("click", (e) => {
  const menu = $("main").querySelector(".menu")
  if (menu.open && !menu.contains(e.target)) menu.open = false
})
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("eod").hidden) {
    e.preventDefault()
    closeStopDialog()
    return
  }
  if (
    e.key === " " &&
    state?.signedIn &&
    $("perm").hidden &&
    $("eod").hidden &&
    !["INPUT", "TEXTAREA", "BUTTON"].includes(document.activeElement?.tagName)
  ) {
    e.preventDefault()
    $("toggle").click()
  }
})

listen("state", (e) => render(e.payload))
render(await invoke("get_state"))
