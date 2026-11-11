// Small pure helpers used by app.js (kept separate so they can be unit-tested).

export function formatClock(seconds) {
  const s = Math.max(0, Math.floor(seconds))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`
}

/** Tasks of the chosen project (all tasks when none), split into mine / others. */
export function groupTasks(tasks, projectId) {
  const list = projectId ? tasks.filter((t) => t.projectId === projectId) : tasks
  return { mine: list.filter((t) => t.mine), others: list.filter((t) => !t.mine) }
}
