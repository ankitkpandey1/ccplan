"use strict";

// Tauri v2 exposes the API on window.__TAURI__ when withGlobalTauri is set.
const invoke =
  window.__TAURI__ && window.__TAURI__.core
    ? window.__TAURI__.core.invoke
    : async () => {
        throw new Error("Tauri bridge unavailable (open this through the app, not a browser).");
      };

const state = {
  view: "today",
  date: null, // null = today; otherwise "YYYY-MM-DD"
  snapshot: null,
};

const $ = (sel) => document.querySelector(sel);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
};

const VIEW_TITLES = {
  today: "Today",
  upcoming: "Upcoming",
  automations: "Automations",
  approvals: "Approvals",
  activity: "Activity",
  agents: "Agents",
};

/* ---------------- data ---------------- */

async function refresh(mutation) {
  try {
    const snap = mutation
      ? await mutation()
      : await invoke("snapshot", { date: state.date });
    state.snapshot = snap;
    render();
  } catch (err) {
    toast(String(err), true);
  }
}

/* ---------------- rendering ---------------- */

function render() {
  const snap = state.snapshot;
  if (!snap) return;

  // nav active
  document.querySelectorAll(".nav-item").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === state.view)
  );

  // approvals badge
  const badge = $("#approvalsBadge");
  if (snap.pending_approvals > 0) {
    badge.hidden = false;
    badge.textContent = String(snap.pending_approvals);
  } else {
    badge.hidden = true;
  }

  // title + subtitle + date controls
  $("#viewTitle").textContent = VIEW_TITLES[state.view];
  const sub = $("#viewSub");
  const dateNav = state.view === "today" || state.view === "upcoming";
  $("#prevDay").style.visibility = dateNav ? "visible" : "hidden";
  $("#nextDay").style.visibility = dateNav ? "visible" : "hidden";
  $("#todayBtn").hidden = !dateNav || snap.is_today;
  if (state.view === "today") {
    sub.textContent = `${humanDate(snap.date)} · ${snap.today.now_label} now`;
  } else {
    sub.textContent = "";
  }

  const content = $("#content");
  content.innerHTML = "";
  content.appendChild(VIEWS[state.view](snap));

  renderRail(snap);
}

const VIEWS = {
  today: renderToday,
  upcoming: renderUpcoming,
  automations: renderAutomations,
  approvals: renderApprovals,
  activity: renderActivity,
  agents: renderAgents,
};

function renderToday(snap) {
  const cards = snap.today.cards;
  if (cards.length === 0) {
    return emptyState(
      "◎",
      "Nothing scheduled",
      "Your day is clear. Add a block to start shaping it.",
      true
    );
  }
  const wrap = el("div", "timeline");
  cards.forEach((card, i) => {
    if (snap.today.now_line_index === i) wrap.appendChild(nowLine(snap.today.now_label));
    wrap.appendChild(blockCard(card, true));
  });
  if (snap.today.now_line_index === null) wrap.appendChild(nowLine(snap.today.now_label));
  return wrap;
}

function renderUpcoming(snap) {
  const days = snap.upcoming.days;
  if (days.length === 0) {
    return emptyState("▦", "Nothing upcoming", "No plans on the days ahead.", false);
  }
  const wrap = el("div");
  days.forEach((day) => {
    const group = el("div", "day-group");
    group.appendChild(el("h4", null, humanDate(day.date_label)));
    const tl = el("div", "timeline");
    day.cards.forEach((c) => tl.appendChild(blockCard(c, false)));
    group.appendChild(tl);
    wrap.appendChild(group);
  });
  return wrap;
}

function renderAutomations(snap) {
  const rules = snap.automations.rules;
  if (rules.length === 0) {
    return emptyState(
      "↻",
      "No automations",
      "Recurring blocks you create will show up here.",
      false
    );
  }
  const rows = el("div", "rows");
  rules.forEach((r) => {
    const row = el("div", "row");
    row.appendChild(el("div", "row-ico", "↻"));
    const main = el("div", "row-main");
    main.appendChild(el("div", "row-title", r.title));
    const subParts = [r.schedule];
    if (r.end_label) subParts.push(r.end_label);
    if (r.has_run) subParts.push("runs command");
    if (r.has_agent) subParts.push("agent");
    main.appendChild(el("div", "row-sub", subParts.join(" · ")));
    row.appendChild(main);
    rows.appendChild(row);
  });
  return rows;
}

function renderApprovals(snap) {
  const items = snap.approvals.items;
  if (items.length === 0) {
    return emptyState(
      "⎙",
      "Nothing to approve",
      "Blocks that want to run a command will wait here for your OK.",
      false
    );
  }
  const rows = el("div", "rows");
  items.forEach((it) => {
    const row = el("div", "row");
    const main = el("div", "row-main");
    main.appendChild(el("div", "row-title", `${it.title}  ·  ${it.when}`));
    if (it.reason) main.appendChild(el("div", "row-sub", it.reason));
    if (it.argv) main.appendChild(el("div", "approval-argv", it.argv));
    row.appendChild(main);

    const actions = el("div", "approval-actions");
    const ok = el("button", "primary-btn", "Approve");
    ok.onclick = () => refresh(() => invoke("approve_block", { id: it.id, date: state.date }));
    const no = el("button", "ghost-btn", "Deny");
    no.style.width = "auto";
    no.onclick = () => refresh(() => invoke("remove_block", { id: it.id, date: state.date }));
    actions.append(ok, no);
    row.appendChild(actions);
    rows.appendChild(row);
  });
  return rows;
}

function renderActivity(snap) {
  const items = snap.activity.items;
  if (items.length === 0) {
    return emptyState("≣", "No activity yet", "Scheduler events will appear here as they fire.", false);
  }
  const rows = el("div", "rows");
  items.forEach((it) => {
    const row = el("div", `row k-${it.kind}`);
    row.appendChild(el("div", "row-ico", it.icon));
    const main = el("div", "row-main");
    main.appendChild(el("div", "row-title", it.text));
    row.appendChild(main);
    row.appendChild(el("div", "row-time", it.ts_label));
    rows.appendChild(row);
  });
  return rows;
}

function renderAgents(snap) {
  const agents = snap.agents.agents;
  if (agents.length === 0) {
    return emptyState("⚡", "No agents", "Blocks assigned to an agent will report their status here.", false);
  }
  const rows = el("div", "rows");
  agents.forEach((a) => {
    const row = el("div", "row");
    const dot = el("div", `dot ${a.is_ok ? "ok" : "bad"}`);
    row.appendChild(dot);
    const main = el("div", "row-main");
    main.appendChild(el("div", "row-title", a.name));
    main.appendChild(el("div", "row-sub", a.last_action));
    row.appendChild(main);
    rows.appendChild(row);
  });
  return rows;
}

/* ---------------- pieces ---------------- */

function nowLine(label) {
  return el("div", "now-line", label);
}

function blockCard(card, actionable) {
  const terminal = ["done", "skipped", "missed", "expired"].includes(card.status);
  const isNow = card.countdown === "now";
  const node = el("div", `card${isNow ? " is-now" : ""}${terminal ? " terminal" : ""}`);

  const time = el("div", "card-time");
  time.appendChild(el("div", "card-range", card.time_range));
  time.appendChild(el("div", "card-count", card.countdown));
  node.appendChild(time);

  const body = el("div", "card-body");
  body.appendChild(el("div", "card-title", card.title));
  const meta = el("div", "card-meta");
  meta.appendChild(el("span", `pill status-${card.status}`, statusLabel(card.status)));
  card.tags.forEach((t) => meta.appendChild(el("span", "pill tag", `#${t}`)));
  if (card.has_recurrence) meta.appendChild(el("span", "pill glyph", "↻"));
  if (card.has_run) meta.appendChild(el("span", "pill glyph", "▶"));
  if (card.has_agent) meta.appendChild(el("span", "pill glyph", "⚡"));
  if (card.awaiting_approval) meta.appendChild(el("span", "pill warn", "needs approval"));
  if (card.has_expect_by_breach) meta.appendChild(el("span", "pill warn", "overdue"));
  body.appendChild(meta);
  node.appendChild(body);

  if (actionable) {
    const actions = el("div", "card-actions");
    if (!terminal) {
      actions.appendChild(
        iconBtn("✓", "done", "Mark done", () =>
          refresh(() => invoke("mark_block", { id: card.id, action: "done", date: state.date }))
        )
      );
      actions.appendChild(
        iconBtn("⤼", "", "Skip", () =>
          refresh(() => invoke("mark_block", { id: card.id, action: "skip", date: state.date }))
        )
      );
      actions.appendChild(
        iconBtn("☾", "", "Snooze 10m", () =>
          refresh(() => invoke("snooze_block", { id: card.id, by: "10m", date: state.date }))
        )
      );
    }
    actions.appendChild(
      iconBtn("✕", "danger", "Delete", () =>
        refresh(() => invoke("remove_block", { id: card.id, date: state.date }))
      )
    );
    node.appendChild(actions);
  }
  return node;
}

function iconBtn(glyph, extra, title, onclick) {
  const b = el("button", `icon-btn ${extra}`.trim(), glyph);
  b.title = title;
  b.onclick = onclick;
  return b;
}

function statusLabel(s) {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function emptyState(glyph, title, body, showCta) {
  const e = el("div", "empty");
  e.appendChild(el("div", "glyph", glyph));
  e.appendChild(el("h3", null, title));
  e.appendChild(el("p", null, body));
  if (showCta) {
    const cta = el("button", "primary-btn", "＋ New block");
    cta.onclick = openSheet;
    e.appendChild(cta);
  }
  return e;
}

function renderRail(snap) {
  const rail = $("#rail");
  rail.innerHTML = "";
  const items = snap.up_next.items;
  if (items.length === 0) {
    rail.appendChild(el("div", "rail-empty", "Nothing coming up."));
    return;
  }
  items.forEach((c) => {
    const card = el("div", "rail-card");
    card.appendChild(el("div", "t", c.title));
    card.appendChild(el("div", "m", `${c.time_range} · ${c.countdown}`));
    rail.appendChild(card);
  });
}

/* ---------------- date helpers ---------------- */

function humanDate(iso) {
  const [y, m, d] = iso.split("-").map(Number);
  const dt = new Date(Date.UTC(y, m - 1, d));
  return dt.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
    timeZone: "UTC",
  });
}

function shiftDate(iso, days) {
  const [y, m, d] = iso.split("-").map(Number);
  const dt = new Date(Date.UTC(y, m - 1, d));
  dt.setUTCDate(dt.getUTCDate() + days);
  return dt.toISOString().slice(0, 10);
}

/* ---------------- sheet ---------------- */

function openSheet() {
  $("#sheetError").hidden = true;
  $("#f_title").value = "";
  $("#f_tags").value = "";
  $("#f_start").value = nextHalfHour();
  $("#f_duration").value = "30m";
  $("#sheetBackdrop").hidden = false;
  setTimeout(() => $("#f_title").focus(), 30);
}
function closeSheet() {
  $("#sheetBackdrop").hidden = true;
}
function nextHalfHour() {
  const now = new Date();
  let h = now.getHours();
  let m = now.getMinutes() < 30 ? 30 : 0;
  if (m === 0) h = (h + 1) % 24;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

async function submitBlock(ev) {
  ev.preventDefault();
  const title = $("#f_title").value.trim();
  const start = $("#f_start").value;
  if (!title || !start) return;
  const tags = $("#f_tags").value
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  try {
    const snap = await invoke("add_block", {
      date: state.date,
      title,
      start,
      duration: $("#f_duration").value,
      end: null,
      tags,
    });
    state.snapshot = snap;
    closeSheet();
    render();
    toast("Block added");
  } catch (err) {
    const e = $("#sheetError");
    e.hidden = false;
    e.textContent = String(err);
  }
}

/* ---------------- toast ---------------- */

let toastTimer = null;
function toast(msg, isError) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = `toast${isError ? " error" : ""}`;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isError ? 4000 : 1800);
}

/* ---------------- wiring ---------------- */

document.querySelectorAll(".nav-item").forEach((b) => {
  b.onclick = () => {
    state.view = b.dataset.view;
    render();
  };
});

$("#newBlockBtn").onclick = openSheet;
$("#sheetCancel").onclick = closeSheet;
$("#blockForm").onsubmit = submitBlock;
$("#sheetBackdrop").onclick = (e) => {
  if (e.target === $("#sheetBackdrop")) closeSheet();
};
$("#refreshBtn").onclick = () => refresh();

$("#prevDay").onclick = () => {
  state.date = shiftDate(state.snapshot.date, -1);
  refresh();
};
$("#nextDay").onclick = () => {
  state.date = shiftDate(state.snapshot.date, 1);
  refresh();
};
$("#todayBtn").onclick = () => {
  state.date = null;
  refresh();
};

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeSheet();
  if (e.key === "n" && !$("#sheetBackdrop").hidden === false && e.target.tagName !== "INPUT") {
    openSheet();
  }
});

refresh();
