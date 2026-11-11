import { pickLang, translator } from "./i18n.js"
import { activityBand, formatClock, formatHM, groupTasks, permissionsBlock, todayRows } from "./logic.js"

const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const lang = pickLang(navigator.languages)
const t = translator(lang)
document.documentElement.lang = lang
if (/Mac/.test(navigator.userAgent)) document.body.classList.add("mac")

const $ = (id) => document.getElementById(id)
for (const el of document.querySelectorAll("[data-t]")) el.textContent = t(el.dataset.t)
for (const el of document.querySelectorAll("[data-t-title]")) el.title = t(el.dataset.tTitle)
$("new-task-title").placeholder = t("newTask")

let state = null
const timeFmt = new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit" })
const KNOWN_ERRORS = ["offline", "invalidServer", "signOutFirst", "emptyTitle", "permissionsRequired", "updateFailed"]

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

function option(value, label, selected) {
  return el("option", { value, textContent: label, selected })
}

let lastCatalogKey = ""
function renderPickers(s) {
  const catalog = s.catalog ?? { projects: [], tasks: [] }
  const key = JSON.stringify([catalog.projects, catalog.tasks.map((x) => x.id), s.projectId, s.taskId])
  if (key === lastCatalogKey) return
  lastCatalogKey = key
  $("project").replaceChildren(
    option("", t("noProject"), !s.projectId),
    ...catalog.projects.map((p) => option(p.id, p.name, p.id === s.projectId)),
  )
  const { mine, others } = groupTasks(catalog.tasks, s.projectId)
  const groups = []
  for (const [label, list] of [
    [t("mine"), mine],
    [t("others"), others],
  ]) {
    if (!list.length) continue
    const g = el("optgroup", { label })
    g.append(...list.map((x) => option(x.id, x.title, x.id === s.taskId)))
    groups.push(g)
  }
  $("task").replaceChildren(option("", t("noTask"), !s.taskId), ...groups)
}

function currentLabel(s) {
  const task = s.catalog?.tasks.find((x) => x.id === s.taskId)
  const project = s.catalog?.projects.find((x) => x.id === s.projectId)
  return [project?.name, task?.title].filter(Boolean).join(" · ") || t("noTask")
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
  const rows = todayRows(s.todayTasks)
  $("today-total").textContent = formatHM(s.todaySeconds)
  $("today-empty").hidden = rows.length > 0
  $("today-list").replaceChildren(
    ...rows.map((r) => {
      const live = s.tracking && (r.taskId ?? null) === (s.taskId ?? null)
      const name = el("span", { className: r.title ? "name" : "name none" })
      if (live) name.append(el("i", { className: "live" }))
      name.append(r.title ?? t("noTask"))
      const share = el("span", { className: "share" }, el("i"))
      share.firstChild.style.width = `${Math.round(r.share * 100)}%`
      return el("li", {}, name, el("span", { className: "dur", textContent: formatHM(r.seconds) }), share)
    }),
  )
}

function render(s) {
  const policyChanged = JSON.stringify(state?.policy) !== JSON.stringify(s.policy) || state?.signedIn !== s.signedIn
  state = s
  for (const v of document.querySelectorAll(".version-line")) v.textContent = `Sentroy Monitor v${s.version}`
  const screen = !s.consentAccepted ? "consent" : !s.signedIn ? "signin" : "main"
  for (const id of ["consent", "signin", "main"]) $(id).hidden = id !== screen

  if (screen === "signin") {
    $("signin-idle").hidden = !!s.login
    $("signin-pending").hidden = !s.login
    if (s.login) $("user-code").textContent = s.login.userCode
    if (document.activeElement !== $("server")) $("server").value = s.serverUrl
  }

  if (screen === "main") {
    $("company").textContent = s.companyName ?? "Sentroy Monitor"
    $("user").textContent = s.userName ?? ""
    renderPickers(s)
    const toggle = $("toggle")
    toggle.setAttribute("aria-pressed", String(s.tracking))
    toggle.setAttribute("aria-label", s.tracking ? t("stop") : t("start"))
    toggle.title = s.tracking ? t("stop") : t("start")
    $("clock").textContent = formatClock(s.sessionSeconds)
    $("current").textContent = currentLabel(s)
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
}

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
  $("perm-restart").hidden = !items.some((p) => p.required && !p.granted && p.needsRestart)
  $("perm-continue").disabled = blocking
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
$("login").addEventListener("click", () => call("start_login"))
$("open-again").addEventListener("click", () => call("open_login_page"))
$("cancel-login").addEventListener("click", () => call("cancel_login"))
$("server-form").addEventListener("submit", (e) => {
  e.preventDefault()
  call("set_server", { url: $("server").value })
})
$("server").addEventListener("change", () => call("set_server", { url: $("server").value }))
$("toggle").addEventListener("click", () => call(state?.tracking ? "stop_tracking" : "start_tracking"))
$("project").addEventListener("change", (e) => call("select", { projectId: e.target.value || null, taskId: null }))
$("task").addEventListener("change", (e) => {
  const taskId = e.target.value || null
  const task = state?.catalog?.tasks.find((x) => x.id === taskId)
  call("select", { projectId: task?.projectId ?? state?.projectId ?? null, taskId })
})
$("new-task").addEventListener("submit", async (e) => {
  e.preventDefault()
  const input = $("new-task-title")
  const title = input.value.trim()
  if (!title) return notice("emptyTitle")
  input.disabled = true
  await call("create_task", { title })
  input.disabled = false
  input.value = ""
})
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
  if (e.key === " " && state?.signedIn && $("perm").hidden && !["INPUT", "SELECT", "BUTTON"].includes(document.activeElement?.tagName)) {
    e.preventDefault()
    $("toggle").click()
  }
})

listen("state", (e) => render(e.payload))
render(await invoke("get_state"))
