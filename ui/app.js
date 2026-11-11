import { pickLang, translator } from "./i18n.js"
import { formatClock, groupTasks } from "./logic.js"

const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const lang = pickLang(navigator.languages)
const t = translator(lang)
document.documentElement.lang = lang

const $ = (id) => document.getElementById(id)
for (const el of document.querySelectorAll("[data-t]")) el.textContent = t(el.dataset.t)
$("new-task-title").placeholder = t("newTask")

let state = null
const timeFmt = new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit" })

function notice(key) {
  if (!key) {
    $("notice").hidden = true
    return
  }
  $("notice-text").textContent = t(key)
  $("notice").hidden = false
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args)
  } catch (e) {
    const key = String(e)
    notice(["offline", "invalidServer", "signOutFirst", "emptyTitle"].includes(key) ? (key === "offline" ? "offlineError" : key) : null)
    if (!["offline", "invalidServer", "signOutFirst", "emptyTitle"].includes(key)) console.error(e)
    return undefined
  }
}

function option(value, label, selected) {
  const o = document.createElement("option")
  o.value = value
  o.textContent = label
  o.selected = selected
  return o
}

let lastCatalogKey = ""
function renderPickers(s) {
  const catalog = s.catalog ?? { projects: [], tasks: [] }
  const key = JSON.stringify([catalog.projects, catalog.tasks.map((x) => x.id), s.projectId, s.taskId])
  if (key === lastCatalogKey) return
  lastCatalogKey = key
  const project = $("project")
  project.replaceChildren(option("", t("noProject"), !s.projectId), ...catalog.projects.map((p) => option(p.id, p.name, p.id === s.projectId)))
  const { mine, others } = groupTasks(catalog.tasks, s.projectId)
  const task = $("task")
  const groups = []
  for (const [label, list] of [
    [t("mine"), mine],
    [t("others"), others],
  ]) {
    if (!list.length) continue
    const g = document.createElement("optgroup")
    g.label = label
    g.append(...list.map((x) => option(x.id, x.title, x.id === s.taskId)))
    groups.push(g)
  }
  task.replaceChildren(option("", t("noTask"), !s.taskId), ...groups)
}

function render(s) {
  state = s
  $("version").textContent = `v${s.version}`
  const screen = !s.consentAccepted ? "consent" : !s.signedIn ? "signin" : "main"
  for (const id of ["consent", "signin", "main"]) $(id).hidden = id !== screen

  if (screen === "signin") {
    $("signin-idle").hidden = !!s.login
    $("signin-pending").hidden = !s.login
    if (s.login) $("user-code").textContent = s.login.userCode
    if (document.activeElement !== $("server")) $("server").value = s.serverUrl
  }

  if (screen === "main") {
    $("company").textContent = s.companyName ?? ""
    $("user").textContent = s.userName ?? ""
    renderPickers(s)
    const toggle = $("toggle")
    toggle.setAttribute("aria-pressed", String(s.tracking))
    toggle.setAttribute("aria-label", s.tracking ? t("stop") : t("start"))
    toggle.title = s.tracking ? t("stop") : t("start")
    $("clock").textContent = formatClock(s.sessionSeconds)
    $("today").textContent = formatClock(s.todaySeconds).slice(0, 5)
    const pct = s.activityPercent
    $("activity").textContent = pct == null ? "—" : `${pct}%`
    const bar = $("meter-bar")
    bar.style.width = `${pct ?? 0}%`
    bar.style.background = pct == null ? "" : pct >= 60 ? "var(--good)" : pct >= 30 ? "var(--warn)" : "var(--bad)"
    $("dot").className = `dot ${s.online ? "on" : "off"}`
    $("online").textContent = s.online ? t("online") : t("offline")
    const minutes = s.queued
    $("sync").textContent =
      minutes > 1 ? t("queued", { n: minutes }) : s.lastSync ? t("synced", { time: timeFmt.format(new Date(s.lastSync)) }) : ""
  }
  if (s.notice) notice(s.notice)
}

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
document.addEventListener("keydown", (e) => {
  if (e.key === " " && state?.signedIn && !["INPUT", "SELECT", "BUTTON"].includes(document.activeElement?.tagName)) {
    e.preventDefault()
    $("toggle").click()
  }
})

listen("state", (e) => render(e.payload))
render(await invoke("get_state"))
