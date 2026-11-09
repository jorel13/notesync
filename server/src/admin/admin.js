// notesync admin page. Talks to /api/v1/admin with a session cookie; every
// request carries the x-notesync-admin header the server requires.
"use strict";

const API = "/api/v1/admin";
const $ = (id) => document.getElementById(id);

let me = null;
let selected = null;

// ---- Helpers ----

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function api(method, path, body) {
  const options = { method, headers: { "x-notesync-admin": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    options.headers["content-type"] = "application/json";
    options.body = JSON.stringify(body);
  }
  const res = await fetch(API + path, options);
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    const err = (data && data.error) || {};
    if (res.status === 401 && me) {
      showSignIn("Your admin session ended. Sign in again.");
    }
    throw new ApiError(res.status, err.code || "error", err.message || `Request failed (${res.status})`);
  }
  return data;
}

/** Builds an element. Text is always set as text, never as HTML. */
function el(tag, attrs, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (value !== false && value != null) node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child == null || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function fill(tbody, rows, columns, emptyText) {
  tbody.replaceChildren();
  if (rows.length === 0) {
    tbody.append(el("tr", {}, el("td", { colspan: columns, class: "empty" }, emptyText)));
    return;
  }
  for (const row of rows) tbody.append(row);
}

function date(secs) {
  if (!secs) return "—";
  return new Date(secs * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function ago(secs) {
  const diff = Math.round(Date.now() / 1000 - secs);
  if (diff < 90) return "just now";
  if (diff < 5400) return `${Math.round(diff / 60)} min ago`;
  if (diff < 129600) return `${Math.round(diff / 3600)} h ago`;
  return `${Math.round(diff / 86400)} days ago`;
}

function until(secs) {
  const diff = Math.round(secs - Date.now() / 1000);
  if (diff <= 0) return "now";
  if (diff < 5400) return `in ${Math.max(1, Math.round(diff / 60))} min`;
  if (diff < 129600) return `in ${Math.round(diff / 3600)} h`;
  return `in ${Math.round(diff / 86400)} days`;
}

function bytes(n) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 || n >= 100 ? 0 : 1)} ${units[i]}`;
}

function rate(r) {
  const every = r.every_secs >= 1 ? `${r.every_secs} s` : `${Math.round(1 / r.every_secs)} per second`;
  return r.every_secs >= 1 ? `${r.burst} at once, then one every ${every}` : `${r.burst} at once, then ${every}`;
}

let noticeTimer = null;
function notice(text, isError) {
  const box = $("notice");
  box.textContent = text;
  box.classList.toggle("error", !!isError);
  box.hidden = false;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => (box.hidden = true), isError ? 8000 : 4000);
}

/** Runs an action, reporting success or the server's error message. */
async function act(work, success) {
  try {
    await work();
    if (success) notice(success);
    return true;
  } catch (err) {
    if (!(err instanceof ApiError && err.status === 401)) notice(err.message, true);
    return false;
  }
}

// ---- Dialog ----

/**
 * Shows a dialog with fields; resolves to the field values, or null if
 * cancelled. `submit` runs while the dialog is open so errors show in it.
 */
function dialog({ title, text, fields = [], ok = "OK", danger = false, submit }) {
  const box = $("dialog");
  $("dialog-title").textContent = title;
  $("dialog-text").textContent = text || "";
  $("dialog-text").hidden = !text;
  $("dialog-error").hidden = true;
  const okButton = $("dialog-ok");
  okButton.textContent = ok;
  okButton.className = danger ? "primary danger" : "primary";
  const container = $("dialog-fields");
  container.replaceChildren();
  for (const f of fields) {
    if (f.type === "checkbox") {
      container.append(el("label", { class: "check" }, el("input", { type: "checkbox", name: f.name }), f.label));
    } else {
      container.append(
        el("label", {}, f.label, el("input", {
          name: f.name,
          type: f.type || "text",
          autocomplete: f.autocomplete || "off",
          required: f.optional ? false : true,
          minlength: f.minlength || false,
        })),
      );
    }
  }
  return new Promise((resolve) => {
    const form = $("dialog-form");
    const finish = (value) => {
      form.onsubmit = null;
      $("dialog-cancel").onclick = null;
      box.onclose = null;
      if (box.open) box.close();
      resolve(value);
    };
    form.onsubmit = async (event) => {
      event.preventDefault();
      const values = {};
      for (const f of fields) {
        const input = form.elements.namedItem(f.name);
        values[f.name] = f.type === "checkbox" ? input.checked : input.value;
      }
      okButton.disabled = true;
      try {
        if (submit) await submit(values);
        finish(values);
      } catch (err) {
        $("dialog-error").textContent = err.message;
        $("dialog-error").hidden = false;
      } finally {
        okButton.disabled = false;
      }
    };
    $("dialog-cancel").onclick = () => finish(null);
    box.onclose = () => finish(null);
    box.showModal();
    const first = container.querySelector("input");
    if (first) first.focus();
  });
}

const yourPassword = {
  name: "admin_password",
  label: "Your password",
  type: "password",
  autocomplete: "current-password",
};

// ---- Sign-in ----

function showSignIn(message) {
  me = null;
  $("app").hidden = true;
  $("who").hidden = true;
  $("sign-in").hidden = false;
  const error = $("sign-in-error");
  error.textContent = message || "";
  error.hidden = !message;
  $("sign-in-form").elements.namedItem("username").focus();
}

function showApp(session) {
  me = session;
  $("who-name").textContent = session.username;
  $("who").hidden = false;
  $("sign-in").hidden = true;
  $("app").hidden = false;
  showTab("users");
}

$("sign-in-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.target;
  const button = form.querySelector("button");
  button.disabled = true;
  try {
    const session = await api("POST", "/login", {
      username: form.elements.namedItem("username").value.trim(),
      password: form.elements.namedItem("password").value,
    });
    form.reset();
    showApp(session);
  } catch (err) {
    const message = err.status === 401 ? "Wrong username or password." : err.message;
    showSignIn(message);
  } finally {
    button.disabled = false;
  }
});

$("sign-out").addEventListener("click", async () => {
  await act(() => api("POST", "/logout"));
  showSignIn();
});

// ---- Tabs ----

function showTab(name) {
  for (const tab of document.querySelectorAll(".tabs button")) {
    tab.setAttribute("aria-selected", String(tab.dataset.tab === name));
  }
  $("tab-users").hidden = name !== "users";
  $("tab-server").hidden = name !== "server";
  if (name === "users") loadUsers();
  else loadServer();
}

for (const tab of document.querySelectorAll(".tabs button")) {
  tab.addEventListener("click", () => showTab(tab.dataset.tab));
}

// ---- Users ----

function statusBadge(user) {
  const now = Date.now() / 1000;
  if (user.disabled) return el("span", { class: "badge bad" }, "Disabled");
  if (user.locked_until > now) return el("span", { class: "badge warn" }, `Locked, ends ${until(user.locked_until)}`);
  return el("span", { class: "badge ok" }, "Active");
}

async function loadUsers() {
  let users;
  try {
    users = await api("GET", "/users");
  } catch (err) {
    if (err.status !== 401) notice(err.message, true);
    return;
  }
  const rows = users.map((u) =>
    el(
      "tr",
      {
        class: selected === u.username ? "selected" : "",
        tabindex: "0",
        onclick: () => openUser(u.username),
        onkeydown: (e) => e.key === "Enter" && openUser(u.username),
      },
      el("td", {}, u.username),
      el("td", {}, u.is_admin ? "Admin" : "User"),
      el("td", {}, statusBadge(u)),
      el("td", { class: "num" }, u.devices),
      el("td", {}, date(u.created_at)),
    ),
  );
  fill($("users-table").querySelector("tbody"), rows, 5, "No users yet.");
  if (selected) {
    if (users.some((u) => u.username === selected)) await openUser(selected);
    else closeUser();
  }
}

function closeUser() {
  selected = null;
  $("user-detail").hidden = true;
  for (const row of $("users-table").querySelectorAll("tr.selected")) row.classList.remove("selected");
}

async function refresh(name) {
  selected = name;
  await loadUsers();
}

async function openUser(name) {
  let detail;
  try {
    detail = await api("GET", `/users/${encodeURIComponent(name)}`);
  } catch (err) {
    if (err.status !== 401) notice(err.message, true);
    return;
  }
  selected = name;
  for (const row of $("users-table").querySelectorAll("tbody tr")) {
    row.classList.toggle("selected", row.firstChild && row.firstChild.textContent === name);
  }
  const u = detail.user;
  const path = `/users/${encodeURIComponent(u.username)}`;
  const isMe = me && u.username.toLowerCase() === me.username.toLowerCase();
  $("detail-name").textContent = u.username;
  $("detail-summary").replaceChildren(
    `${u.is_admin ? "Admin" : "User"} since ${date(u.created_at)} · `,
    statusBadge(u),
  );

  const actions = [];
  actions.push(
    el("button", {
      onclick: () =>
        dialog({
          title: `Set a new password for ${u.username}`,
          text: "This signs them out on every device. Give them the new password yourself.",
          fields: [
            { name: "new_password", label: "New password (8 or more characters)", type: "password", autocomplete: "new-password", minlength: 8 },
            yourPassword,
          ],
          ok: "Set password",
          submit: (v) => api("PUT", `${path}/password`, v),
        }).then((v) => v && (notice(`Set a new password for ${u.username}.`), refresh(u.username))),
    }, "Set password"),
  );
  if (u.locked_until > Date.now() / 1000) {
    actions.push(
      el("button", {
        onclick: () =>
          act(() => api("POST", `${path}/unlock`), `Cleared the lockout for ${u.username}.`).then(() => refresh(u.username)),
      }, "Unlock sign-in"),
    );
  }
  if (u.disabled) {
    actions.push(
      el("button", {
        onclick: () =>
          act(() => api("PUT", `${path}/disabled`, { disabled: false }), `Enabled ${u.username}.`).then(() => refresh(u.username)),
      }, "Enable"),
    );
  } else if (!isMe) {
    actions.push(
      el("button", {
        class: "danger",
        onclick: () =>
          dialog({
            title: `Disable ${u.username}?`,
            text: "They can't sign in, and every device they use is signed out. Their vaults are kept.",
            ok: "Disable",
            danger: true,
            submit: () => api("PUT", `${path}/disabled`, { disabled: true }),
          }).then((v) => v && (notice(`Disabled ${u.username}.`), refresh(u.username))),
      }, "Disable"),
    );
  }
  if (!u.is_admin) {
    actions.push(
      el("button", {
        onclick: () =>
          dialog({
            title: `Make ${u.username} an admin?`,
            text: "Admins can manage every account on this server, including yours.",
            fields: [yourPassword],
            ok: "Make admin",
            submit: (v) => api("PUT", `${path}/admin`, { is_admin: true, admin_password: v.admin_password }),
          }).then((v) => v && (notice(`${u.username} is now an admin.`), refresh(u.username))),
      }, "Make admin"),
    );
  } else if (!isMe) {
    actions.push(
      el("button", {
        onclick: () =>
          dialog({
            title: `Remove admin rights from ${u.username}?`,
            fields: [yourPassword],
            ok: "Remove admin",
            danger: true,
            submit: (v) => api("PUT", `${path}/admin`, { is_admin: false, admin_password: v.admin_password }),
          }).then((v) => v && (notice(`${u.username} is no longer an admin.`), refresh(u.username))),
      }, "Remove admin"),
    );
  }
  if (!isMe) {
    const vaultCount = detail.vaults.length + detail.deleted_vaults.length;
    actions.push(
      el("button", {
        class: "danger",
        onclick: () =>
          dialog({
            title: `Delete ${u.username}?`,
            text: `This permanently deletes the account, its ${vaultCount} vault(s) and every stored file and old version. It can't be undone. Their devices keep their local copies. To only block them, use Disable instead.`,
            fields: [yourPassword],
            ok: "Delete user",
            danger: true,
            submit: (v) => api("DELETE", path, v),
          }).then((v) => {
            if (!v) return;
            notice(`Deleted ${u.username}.`);
            closeUser();
            loadUsers();
          }),
      }, "Delete user"),
    );
  }
  $("detail-actions").replaceChildren(...actions);

  fill(
    $("detail-devices").querySelector("tbody"),
    detail.devices.map((d) =>
      el(
        "tr",
        {},
        el("td", {}, d.name),
        el("td", {}, ago(d.last_seen)),
        el("td", {}, until(d.expires_at)),
        el("td", { class: "num" }, el("button", {
          class: "small danger",
          onclick: () =>
            dialog({
              title: `Sign out "${d.name}"?`,
              text: "The device has to sign in again to keep syncing.",
              ok: "Sign out",
              danger: true,
              submit: () => api("DELETE", `${path}/devices/${d.id}`),
            }).then((v) => v && (notice(`Signed out ${d.name}.`), refresh(u.username))),
        }, "Sign out")),
      ),
    ),
    4,
    "No signed-in devices.",
  );

  fill(
    $("detail-vaults").querySelector("tbody"),
    detail.vaults.map((v) =>
      el(
        "tr",
        {},
        el("td", {}, v.name),
        el("td", { class: "num" }, v.files),
        el("td", { class: "num" }, bytes(v.bytes)),
        el("td", { class: "num" }, v.head_seq),
      ),
    ),
    4,
    "No vaults.",
  );

  fill(
    $("detail-deleted").querySelector("tbody"),
    detail.deleted_vaults.map((v) =>
      el(
        "tr",
        {},
        el("td", {}, v.name),
        el("td", {}, date(v.deleted_at)),
        el("td", {}, until(v.removed_at)),
        el("td", { class: "num" }, el("button", {
          class: "small",
          onclick: () =>
            act(() => api("POST", `${path}/vaults/${v.id}/restore`), `Restored ${v.name}.`).then(() => refresh(u.username)),
        }, "Restore")),
      ),
    ),
    4,
    "None.",
  );

  $("user-detail").hidden = false;
}

$("detail-close").addEventListener("click", closeUser);

$("add-user").addEventListener("click", async () => {
  const result = await dialog({
    title: "Add a user",
    fields: [
      { name: "username", label: "Username (letters, digits, . _ -)", autocomplete: "off" },
      { name: "password", label: "Password (8 or more characters)", type: "password", autocomplete: "new-password", minlength: 8 },
      { name: "is_admin", label: "Admin", type: "checkbox" },
      { ...yourPassword, label: "Your password (only needed to add an admin)", optional: true },
    ],
    ok: "Add user",
    submit: (v) => api("POST", "/users", { ...v, username: v.username.trim() }),
  });
  if (result) {
    notice(`Added ${result.username.trim()}.`);
    await refresh(result.username.trim());
  }
});

// ---- Server ----

async function loadServer() {
  let s;
  try {
    s = await api("GET", "/overview");
  } catch (err) {
    if (err.status !== 401) notice(err.message, true);
    return;
  }
  const stat = (label, value, sub) =>
    el("div", { class: "stat" }, el("div", { class: "value" }, value), el("div", { class: "label" }, label), sub ? el("div", { class: "sub" }, sub) : null);
  $("server-stats").replaceChildren(
    stat("Server version", s.version, `Plugins ${s.min_client_version} or newer`),
    stat("Running since", ago(s.started_at), date(s.started_at)),
    stat("Users", s.users, `${s.admins} admin · ${s.disabled_users} disabled · ${s.locked_users} locked`),
    stat("Signed-in devices", s.devices),
    stat("Vaults", s.vaults, `${s.deleted_vaults} deleted, restorable`),
    stat("Current files", s.files, bytes(s.file_bytes)),
    stat("Stored, with history", bytes(s.stored_bytes), "File contents in blobs/"),
    stat("Database", bytes(s.database_bytes)),
  );
  const c = s.settings;
  const rows = [
    ["Data folder", s.data_dir],
    ["Largest file", bytes(c.max_file_bytes)],
    ["History kept", `${c.history_days} days`],
    ["Device sign-ins last", `${c.token_days} days without syncing`],
    ["Sign-ins per username", rate(c.login_rate)],
    ["Syncs per device", rate(c.sync_rate)],
    ["Requests per device", rate(c.request_rate)],
  ];
  $("server-settings").replaceChildren(...rows.flatMap(([k, v]) => [el("dt", {}, k), el("dd", {}, v)]));
}

$("run-gc").addEventListener("click", async (event) => {
  const button = event.target;
  button.disabled = true;
  let report;
  const ok = await act(async () => {
    report = await api("POST", "/gc");
  });
  button.disabled = false;
  if (ok) {
    notice(`Cleanup removed ${report.blobs_removed} old file versions and ${report.vaults_removed} deleted vaults.`);
    loadServer();
  }
});

// ---- Start ----

api("GET", "/session").then(showApp, (err) => showSignIn(err.status === 401 ? "" : err.message));
