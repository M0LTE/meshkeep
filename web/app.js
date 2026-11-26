// SPDX-License-Identifier: AGPL-3.0-or-later
"use strict";

const MAX_TEXT = 160;
const PAGE = 100;
const $ = (id) => document.getElementById(id);

const S = {
  status: {},
  contacts: [],
  channels: [],
  conversations: [],
  open: null,
  msgs: [],
  hasMore: false,
  filter: "",
};

const readKey = (conv) => "mk.read." + conv;
const lastRead = (conv) => Number(localStorage.getItem(readKey(conv)) || 0);
const markRead = (conv, id) => {
  if (id > lastRead(conv)) localStorage.setItem(readKey(conv), String(id));
};

async function api(path, opts) {
  const r = await fetch(path, Object.assign({ credentials: "same-origin" }, opts));
  if (r.status === 401) {
    location.href = "/login";
    throw new Error("signed out");
  }
  return r;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function hue(s) {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 360;
}

function avatar(name) {
  const a = el("span", "avatar", (name.trim()[0] || "?").toUpperCase());
  a.style.background = `hsl(${hue(name)} 45% 42%)`;
  return a;
}

function contactFor(conv) {
  if (!conv.startsWith("dm:")) return null;
  const p = conv.slice(3);
  return S.contacts.find((c) => c.prefix === p) || null;
}

function channelFor(conv) {
  if (!conv.startsWith("ch:")) return null;
  const i = Number(conv.slice(3));
  return S.channels.find((c) => c.idx === i) || null;
}

function convName(conv) {
  const c = contactFor(conv);
  if (c) return c.name;
  const ch = channelFor(conv);
  if (ch) return ch.name;
  if (conv.startsWith("ch:")) return "Channel " + conv.slice(3);
  return "Unknown " + conv.slice(3);
}

function fmtTime(secs) {
  const d = new Date(secs * 1000);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString([], { day: "numeric", month: "short" });
}

function fmtDay(secs) {
  const d = new Date(secs * 1000);
  const today = new Date();
  const y = new Date(today);
  y.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === y.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short", year: "numeric" });
}

// ---------------------------------------------------------------- state

async function loadState() {
  const r = await api("/api/state");
  const s = await r.json();
  S.status = s.status;
  S.contacts = s.contacts;
  S.channels = s.channels;
  S.conversations = s.conversations;
  renderHeader();
  renderList();
  if (S.open) renderChatTitle();
}

let stateTimer = null;
function refreshSoon() {
  clearTimeout(stateTimer);
  stateTimer = setTimeout(() => loadState().catch(() => {}), 250);
}

function renderHeader() {
  const st = S.status;
  $("node-name").textContent = st.name || "meshkeep";
  const dot = $("radio-dot");
  dot.className = "dot " + (st.radio_connected ? "on" : "off");
  dot.title = st.radio_connected ? "Radio connected" : "Radio not connected";
  const apps = st.apps.length;
  const parts = [st.radio_connected ? "Radio connected" : "Radio offline"];
  if (apps) parts.push(apps === 1 ? "1 app" : apps + " apps");
  $("node-sub").textContent = parts.join(", ");
  if (document.body.dataset.auth !== "1") $("logout-form").hidden = true;
}

// Contact types (ADV_TYPE_*): 1 chat, 2 repeater, 3 room server, 4 sensor.
const KIND_CHAT = 1;
const KIND_ROOM = 3;

function rows() {
  const last = new Map(S.conversations.map((c) => [c.conv, c]));
  const chans = S.channels.map((c) => ({ conv: "ch:" + c.idx, name: c.name, last: last.get("ch:" + c.idx) }));
  for (const c of S.conversations) {
    if (c.conv.startsWith("ch:") && !chans.some((x) => x.conv === c.conv)) {
      chans.push({ conv: c.conv, name: convName(c.conv), last: c });
    }
  }
  const direct = new Map();
  const rooms = new Map();
  const others = new Map();
  const groupFor = (kind) => (kind === KIND_ROOM ? rooms : kind === KIND_CHAT ? direct : others);
  for (const c of S.conversations) {
    if (!c.conv.startsWith("dm:")) continue;
    const ct = contactFor(c.conv);
    // Unknown senders are treated as people.
    groupFor(ct ? ct.kind : KIND_CHAT).set(c.conv, { conv: c.conv, name: convName(c.conv), last: c });
  }
  for (const c of S.contacts) {
    const conv = "dm:" + c.prefix;
    // Repeaters and sensors only appear once there is a conversation.
    if (c.kind !== KIND_CHAT && c.kind !== KIND_ROOM) continue;
    const g = groupFor(c.kind);
    if (!g.has(conv)) g.set(conv, { conv, name: c.name, last: null });
  }
  const byRecent = (a, b) =>
    (b.last ? b.last.last_id : 0) - (a.last ? a.last.last_id : 0) || a.name.localeCompare(b.name);
  const sorted = (m) => [...m.values()].sort(byRecent);
  return { chans, direct: sorted(direct), rooms: sorted(rooms), others: sorted(others) };
}

function renderList() {
  const list = $("list");
  list.replaceChildren();
  const f = S.filter.toLowerCase();
  const { chans, direct, rooms, others } = rows();
  const add = (title, items) => {
    const shown = items.filter((r) => !f || r.name.toLowerCase().includes(f));
    if (!shown.length) return;
    list.append(el("div", "section", title));
    for (const r of shown) list.append(listItem(r));
  };
  add("Channels", chans);
  add("Direct", direct);
  add("Rooms", rooms);
  add("Repeaters", others);
  if (!list.children.length) list.append(el("div", "section", f ? "No matches" : "Nothing yet"));
}

function listItem(r) {
  const b = el("button", "item");
  b.type = "button";
  if (r.conv === S.open) b.classList.add("active");
  if (r.last && r.last.last_direction === "in" && r.last.last_id > lastRead(r.conv) && r.conv !== S.open) {
    b.classList.add("unread");
  }
  b.append(avatar(r.name));
  const body = el("div", "item-body");
  const top = el("div", "item-top");
  top.append(el("span", "item-name", r.name));
  if (r.last) top.append(el("span", "item-time", fmtTime(r.last.last_at)));
  body.append(top);
  body.append(el("div", "item-last", r.last ? (r.last.last_direction === "out" ? "You: " : "") + r.last.last_text : ""));
  b.append(body);
  b.onclick = () => openConv(r.conv);
  return b;
}

// ---------------------------------------------------------------- chat

function renderChatTitle() {
  $("chat-name").textContent = convName(S.open);
  const c = contactFor(S.open);
  const kind = c && c.kind === KIND_ROOM ? "Room, " : c && c.kind !== KIND_CHAT ? "Repeater, " : "";
  $("chat-sub").textContent = S.open.startsWith("ch:") ? "Channel" : c ? kind + c.pubkey.slice(0, 12) : "Not in contacts";
}

async function openConv(conv) {
  S.open = conv;
  S.msgs = [];
  $("chat").classList.add("open");
  $("app").classList.add("in-chat");
  renderChatTitle();
  renderList();
  $("messages").replaceChildren();
  const r = await api("/api/messages?conv=" + encodeURIComponent(conv) + "&limit=" + PAGE);
  const msgs = await r.json();
  if (S.open !== conv) return;
  S.msgs = msgs;
  S.hasMore = msgs.length === PAGE;
  renderMessages(true);
  updateCount();
  if (window.matchMedia("(min-width: 761px)").matches) $("text").focus();
}

async function loadOlder() {
  if (!S.msgs.length) return;
  const conv = S.open;
  const box = $("messages");
  const before = box.scrollHeight;
  const r = await api(`/api/messages?conv=${encodeURIComponent(conv)}&before=${S.msgs[0].id}&limit=${PAGE}`);
  const older = await r.json();
  if (S.open !== conv) return;
  S.msgs = older.concat(S.msgs);
  S.hasMore = older.length === PAGE;
  renderMessages(false);
  box.scrollTop = box.scrollHeight - before;
}

function statusText(m) {
  switch (m.status) {
    case "sending": return "Sending";
    case "sent":
      if (m.kind !== "channel") return "Sent, waiting for ack";
      if (m.heard === 1) return "Heard 1 repeat";
      if (m.heard > 1) return "Heard " + m.heard + " repeats";
      return "Sent";
    case "delivered": return "Delivered";
    case "unconfirmed": return "Not confirmed";
    case "failed": return "Failed";
    default: return "";
  }
}

function metaText(m) {
  const parts = [new Date(m.received_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })];
  if (m.direction === "in") {
    if (m.path_len === 255) parts.push("direct");
    else if (m.path_len != null) {
      const hops = m.path_len & 63;
      parts.push(hops === 0 ? "direct" : hops === 1 ? "1 hop" : hops + " hops");
    }
    if (m.snr != null) parts.push("SNR " + m.snr + " dB");
  } else {
    const s = statusText(m);
    if (s) parts.push(s);
  }
  return parts.join(", ");
}

function authorName(prefix) {
  const c = S.contacts.find((x) => x.pubkey.startsWith(prefix));
  return c ? c.name : prefix;
}

function msgNode(m, prev) {
  const isCh = m.conv.startsWith("ch:");
  const cont = prev && prev.direction === m.direction && prev.sender === m.sender &&
    m.received_at - prev.received_at < 300;
  const wrap = el("div", "msg " + m.direction + (cont ? " cont" : ""));
  wrap.dataset.id = m.id;
  // Channel senders by name; room posts carry the author's key prefix.
  if (m.direction === "in" && !cont && m.sender) {
    wrap.append(el("div", "who", isCh ? m.sender : authorName(m.sender)));
  }
  let text = m.text;
  if (m.kind === "channel_data") text = "(data)";
  wrap.append(el("div", "bubble", text));
  const meta = el("div", "meta", metaText(m));
  if (m.status === "failed" || m.status === "unconfirmed") meta.classList.add("bad");
  wrap.append(meta);
  return wrap;
}

function renderMessages(scrollToEnd) {
  const box = $("messages");
  box.replaceChildren();
  if (S.hasMore) {
    const more = el("button", "ghost more", "Load earlier");
    more.type = "button";
    more.onclick = loadOlder;
    box.append(more);
  }
  let prev = null;
  for (const m of S.msgs) {
    if (!prev || fmtDay(prev.received_at) !== fmtDay(m.received_at)) {
      box.append(el("div", "day", fmtDay(m.received_at)));
      prev = null;
    }
    box.append(msgNode(m, prev));
    prev = m;
  }
  if (scrollToEnd) box.scrollTop = box.scrollHeight;
  if (S.msgs.length) markRead(S.open, S.msgs[S.msgs.length - 1].id);
}

function onMessage(m) {
  if (m.conv === S.open && !S.msgs.some((x) => x.id === m.id)) {
    const box = $("messages");
    const atEnd = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    const prev = S.msgs[S.msgs.length - 1];
    S.msgs.push(m);
    if (!prev || fmtDay(prev.received_at) !== fmtDay(m.received_at)) {
      box.append(el("div", "day", fmtDay(m.received_at)));
      box.append(msgNode(m, null));
    } else {
      box.append(msgNode(m, prev));
    }
    if (atEnd || m.direction === "out") box.scrollTop = box.scrollHeight;
    if (document.visibilityState === "visible") markRead(m.conv, m.id);
  }
  refreshSoon();
}

function onHeard(id, heard) {
  const m = S.msgs.find((x) => x.id === id);
  if (!m) return;
  m.heard = heard;
  const node = $("messages").querySelector(`.msg[data-id="${id}"] .meta`);
  if (node) node.textContent = metaText(m);
}

function onStatus(id, status) {
  const m = S.msgs.find((x) => x.id === id);
  if (!m) return;
  m.status = status;
  const node = $("messages").querySelector(`.msg[data-id="${id}"] .meta`);
  if (node) {
    node.textContent = metaText(m);
    node.classList.toggle("bad", status === "failed" || status === "unconfirmed");
  }
}

// ---------------------------------------------------------------- compose

function limit() {
  if (S.open && S.open.startsWith("ch:")) {
    const name = new TextEncoder().encode(S.status.name || "").length;
    return MAX_TEXT - name - 2;
  }
  return MAX_TEXT;
}

function updateCount() {
  const t = $("text");
  const n = new TextEncoder().encode(t.value.trim()).length;
  const left = limit() - n;
  const c = $("count");
  c.textContent = left < 30 ? String(left) : "";
  c.classList.toggle("bad", left < 0);
  $("send").disabled = n === 0 || left < 0;
  t.style.height = "auto";
  t.style.height = Math.min(t.scrollHeight, 140) + "px";
}

async function send(e) {
  e.preventDefault();
  const t = $("text");
  const text = t.value.trim();
  if (!text || !S.open) return;
  $("send").disabled = true;
  try {
    const r = await api("/api/send", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ conv: S.open, text }),
    });
    if (r.ok) {
      t.value = "";
    } else {
      const body = await r.json().catch(() => ({ error: "Not sent" }));
      const c = $("count");
      c.textContent = body.error;
      c.classList.add("bad");
      return;
    }
  } finally {
    updateCount();
  }
}

// ---------------------------------------------------------------- live

let wsDelay = 1000;
function connect() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/api/events`);
  ws.onopen = () => {
    wsDelay = 1000;
    loadState().catch(() => {});
    if (S.open) openConv(S.open);
  };
  ws.onmessage = (ev) => {
    const e = JSON.parse(ev.data);
    switch (e.type) {
      case "message": onMessage(e.message); break;
      case "message_status": onStatus(e.id, e.status); break;
      case "message_heard": onHeard(e.id, e.heard); break;
      case "drain_done": break;
      default: refreshSoon();
    }
  };
  ws.onclose = () => {
    S.status.radio_connected = false;
    setTimeout(connect, wsDelay);
    wsDelay = Math.min(wsDelay * 2, 30000);
  };
}

$("filter").oninput = (e) => { S.filter = e.target.value; renderList(); };
$("composer").onsubmit = send;
$("text").oninput = updateCount;
$("text").onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
};
$("back").onclick = () => {
  $("app").classList.remove("in-chat");
  S.open = null;
  $("chat").classList.remove("open");
  renderList();
};
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && S.open && S.msgs.length) {
    markRead(S.open, S.msgs[S.msgs.length - 1].id);
    renderList();
  }
});

loadState().catch(() => {});
connect();
