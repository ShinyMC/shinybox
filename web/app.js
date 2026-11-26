// shinybox web interface: a tiny router over four views, talking to /api.

const STORAGE_KEY = "shinybox.boxes";
const TOKEN_KEY = "shinybox.upload-token";
const EXPIRY_PRESETS = [60 * 60, 24 * 60 * 60, 7 * 24 * 60 * 60, 30 * 24 * 60 * 60];
const CHUNK_RETRIES = 5;

const $ = (id) => document.getElementById(id);

const state = {
  info: null,
  files: [],
  expiry: null,
  uploading: false,
};

// ---------------------------------------------------------------------------
// Formatting

function formatBytes(bytes) {
  if (bytes < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = -1;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toFixed(1).replace(/\.0$/, "")} ${units[unit]}`;
}

function plural(count, word) {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

function formatDuration(seconds) {
  const units = [
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ];
  for (const [word, size] of units) {
    if (seconds >= size && seconds % size === 0) return plural(seconds / size, word);
  }
  return plural(Math.round(seconds / 60), "minute");
}

function formatRemaining(seconds) {
  if (seconds < 60) return "under a minute";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} h`;
  return `${Math.floor(seconds / 86400)} d`;
}

function secondsUntil(unixSeconds) {
  return unixSeconds - Math.floor(Date.now() / 1000);
}

function fileType(name) {
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 ? name.slice(dot + 1) : "";
  return extension && extension.length <= 4 ? extension : "file";
}

// Server errors are lowercase fragments; show them as sentences.
function sentence(text) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function filesSummary(count, bytes) {
  return `${plural(count, "file")} · ${formatBytes(bytes)}`;
}

// ---------------------------------------------------------------------------
// DOM helpers

function clone(templateId) {
  return $(templateId).content.firstElementChild.cloneNode(true);
}

function iconButton(icon, label, onClick, extraClass) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = extraClass ? `icon-btn ${extraClass}` : "icon-btn";
  button.setAttribute("aria-label", label);
  button.title = label;
  button.append(clone(icon));
  button.addEventListener("click", onClick);
  return button;
}

function fileRow(name, size, action) {
  const row = clone("file-row");
  row.querySelector(".file-type").textContent = fileType(name);
  row.querySelector(".file-name").textContent = name;
  row.querySelector(".file-name").title = name;
  row.querySelector(".file-size").textContent = formatBytes(size);
  row.querySelector(".file-action").append(action);
  return row;
}

let toastTimer;
function toast(message) {
  const element = $("toast");
  element.textContent = message;
  element.classList.add("is-visible");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => element.classList.remove("is-visible"), 2400);
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("Link copied");
  } catch {
    toast("Could not copy, select the link and copy it manually");
  }
}

// ---------------------------------------------------------------------------
// API

async function api(method, path, { headers = {}, json } = {}) {
  const init = { method, headers: { ...headers } };
  if (json !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(json);
  }
  const response = await fetch(path, init);
  if (response.status === 204) return null;
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(sentence(body.error ?? `request failed (${response.status})`));
    error.status = response.status;
    throw error;
  }
  return body;
}

// Sends one chunk, reporting how many of its bytes are on the way. Resolves with the file's new size.
function sendChunk(url, chunk, deleteToken, onProgress) {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    request.open("PUT", url);
    request.responseType = "json";
    request.setRequestHeader("X-Delete-Token", deleteToken);
    request.upload.addEventListener("progress", (event) => onProgress(event.loaded));
    request.addEventListener("load", () => {
      if (request.status === 200) return resolve(request.response.received);
      const error = new Error(sentence(request.response?.error ?? `upload failed (${request.status})`));
      error.status = request.status;
      reject(error);
    });
    request.addEventListener("error", () => reject(new Error("Network error, please try again")));
    request.send(chunk);
  });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Network errors, conflicts and server errors are worth another try; anything else is final.
function isTransient(error) {
  return error.status === undefined || error.status === 409 || error.status >= 500;
}

// Uploads one file chunk by chunk. After a transient failure it asks the server
// how much arrived and carries on from there, backing off between attempts.
async function uploadFile(upload, index, file, onProgress) {
  const owner = { "X-Delete-Token": upload.delete_token };
  const url = `/api/uploads/${upload.code}/files/${upload.files[index].id}`;
  let offset = 0;
  let failures = 0;

  while (offset < file.size) {
    const chunk = file.slice(offset, offset + upload.chunk_size);
    try {
      offset = await sendChunk(`${url}?offset=${offset}`, chunk, upload.delete_token, (sent) =>
        onProgress(offset + sent),
      );
      failures = 0;
    } catch (error) {
      failures += 1;
      if (!isTransient(error) || failures > CHUNK_RETRIES) throw error;
      await sleep(1000 * 2 ** (failures - 1));
      const progress = await api("GET", `/api/uploads/${upload.code}`, { headers: owner }).catch(() => null);
      if (progress) offset = progress.files[index].received;
    }
    onProgress(offset);
  }
}

// Declares the box, uploads every file and publishes the box. A failed upload is cancelled.
async function uploadBox(files, expiry, token, onProgress) {
  const upload = await api("POST", "/api/uploads", {
    headers: token ? { Authorization: `Bearer ${token}` } : {},
    json: { expires: `${expiry}s`, files: files.map(({ name, size }) => ({ name, size })) },
  });
  const owner = { "X-Delete-Token": upload.delete_token };
  const total = files.reduce((sum, file) => sum + file.size, 0);
  let done = 0;

  try {
    for (const [index, file] of files.entries()) {
      await uploadFile(upload, index, file, (bytes) => onProgress(total ? (done + bytes) / total : 1));
      done += file.size;
    }
    const box = await api("POST", `/api/uploads/${upload.code}/complete`, { headers: owner });
    return { ...box, delete_token: upload.delete_token };
  } catch (error) {
    api("DELETE", `/api/boxes/${upload.code}`, { headers: owner }).catch(() => {});
    throw error;
  }
}

// ---------------------------------------------------------------------------
// Boxes remembered by this browser

function loadBoxes() {
  try {
    const boxes = JSON.parse(localStorage.getItem(STORAGE_KEY));
    return Array.isArray(boxes) ? boxes : [];
  } catch {
    return [];
  }
}

function saveBoxes(boxes) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(boxes));
}

function rememberBox(box) {
  const entry = {
    code: box.code,
    url: box.url,
    deleteToken: box.delete_token,
    expiresAt: box.expires_at,
    fileCount: box.files.length,
    size: box.files.reduce((sum, file) => sum + file.size, 0),
  };
  saveBoxes([entry, ...loadBoxes().filter((saved) => saved.code !== box.code)]);
}

function forgetBox(code) {
  saveBoxes(loadBoxes().filter((saved) => saved.code !== code));
}

// ---------------------------------------------------------------------------
// Views

const VIEWS = ["upload", "done", "box", "mine", "gone"];

function show(view, { title, nav } = {}) {
  for (const name of VIEWS) $(`view-${name}`).hidden = name !== view;
  for (const link of document.querySelectorAll("[data-nav]")) {
    if (link.dataset.nav === nav) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  document.title = title ? `${title} · shinybox` : "shinybox";
}

// Upload

function expiryChoices(info) {
  const choices = EXPIRY_PRESETS.filter((seconds) => seconds <= info.max_expiry);
  if (!choices.includes(info.default_expiry)) choices.push(info.default_expiry);
  return choices.sort((a, b) => a - b);
}

function renderExpiryOptions() {
  const group = $("expiry-options");
  group.replaceChildren();
  for (const seconds of expiryChoices(state.info)) {
    const option = document.createElement("button");
    option.type = "button";
    option.setAttribute("role", "radio");
    option.setAttribute("aria-checked", String(seconds === state.expiry));
    option.tabIndex = seconds === state.expiry ? 0 : -1;
    option.textContent = formatDuration(seconds);
    option.dataset.seconds = String(seconds);
    option.addEventListener("click", () => selectExpiry(seconds));
    group.append(option);
  }
}

function selectExpiry(seconds) {
  state.expiry = seconds;
  renderExpiryOptions();
  $("expiry-options").querySelector('[aria-checked="true"]').focus();
}

function moveExpiry(event) {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
  if (!step) return;
  event.preventDefault();
  const choices = expiryChoices(state.info);
  const next = (choices.indexOf(state.expiry) + step + choices.length) % choices.length;
  selectExpiry(choices[next]);
}

function selectedSize() {
  return state.files.reduce((sum, file) => sum + file.size, 0);
}

function validationError() {
  const { max_files: maxFiles, max_upload_size: maxSize } = state.info;
  if (state.files.length > maxFiles) return `A box holds at most ${plural(maxFiles, "file")}.`;
  if (selectedSize() > maxSize) return `Files add up to more than the ${formatBytes(maxSize)} limit.`;
  if (state.files.some((file) => file.size === 0)) return "Empty files cannot be uploaded.";
  return null;
}

function setUploadError(message) {
  $("upload-error").textContent = message ?? "";
  $("upload-error").hidden = !message;
}

function renderSelection() {
  const list = $("selected-files");
  list.replaceChildren(
    ...state.files.map((file, index) =>
      fileRow(file.name, file.size, iconButton("icon-x", `Remove ${file.name}`, () => removeFile(index))),
    ),
  );
  list.hidden = state.files.length === 0;

  const error = validationError();
  setUploadError(error);
  $("upload-button").disabled = state.uploading || state.files.length === 0 || error !== null;
}

function addFiles(fileList) {
  if (state.uploading) return;
  state.files.push(...fileList);
  renderSelection();
}

function removeFile(index) {
  state.files.splice(index, 1);
  renderSelection();
}

function setUploading(uploading) {
  state.uploading = uploading;
  const form = $("upload-form");
  for (const control of form.querySelectorAll("button, input")) control.disabled = uploading;
  $("dropzone").classList.toggle("is-disabled", uploading);
  $("upload-progress").hidden = !uploading;
  setProgress(0);
  if (!uploading) {
    $("upload-button").textContent = "Upload";
    renderSelection();
  }
}

function setProgress(fraction) {
  $("upload-progress").firstElementChild.style.transform = `scaleX(${fraction})`;
  if (state.uploading) $("upload-button").textContent = `Uploading… ${Math.round(fraction * 100)}%`;
}

async function submitUpload(event) {
  event.preventDefault();
  if (state.uploading || state.files.length === 0 || validationError()) return;

  const token = $("token-input").value.trim();
  if (token) sessionStorage.setItem(TOKEN_KEY, token);

  setUploadError(null);
  setUploading(true);
  try {
    const box = await uploadBox(state.files, state.expiry, token, setProgress);
    rememberBox(box);
    state.files = [];
    setUploading(false);
    showDone(box);
  } catch (error) {
    setUploading(false);
    setUploadError(error.message);
  }
}

function showUpload() {
  show("upload", { nav: "upload" });
  renderSelection();
}

// Upload finished

function showDone(box) {
  const size = box.files.reduce((sum, file) => sum + file.size, 0);
  const lifetime = formatDuration(box.expires_at - box.created_at);
  $("done-summary").textContent = `${filesSummary(box.files.length, size)} · expires in ${lifetime}`;
  $("done-code").textContent = box.code;
  $("done-url").value = box.url;
  show("done", { title: "Box ready", nav: "upload" });
  $("done-copy").focus();
}

// Shared box

async function showBox(code) {
  let box;
  try {
    box = await api("GET", `/api/boxes/${encodeURIComponent(code)}`);
  } catch (error) {
    if (error.status === 404) return showGone();
    throw error;
  }

  $("box-code").textContent = box.code;
  $("box-expiry").textContent = `Expires in ${formatRemaining(secondsUntil(box.expires_at))}`;
  $("box-files").replaceChildren(
    ...box.files.map((file) => {
      const link = document.createElement("a");
      link.className = "icon-btn";
      link.href = `/api/boxes/${box.code}/files/${file.id}`;
      link.download = file.name;
      link.setAttribute("aria-label", `Download ${file.name}`);
      link.title = "Download";
      link.append(clone("icon-download"));
      return fileRow(file.name, file.size, link);
    }),
  );
  const size = box.files.reduce((sum, file) => sum + file.size, 0);
  $("box-summary").textContent = `${filesSummary(box.files.length, size)} · shared with shinybox`;
  show("box", { title: box.code });
}

function showGone() {
  show("gone", { title: "Box not found" });
}

// My boxes

async function deleteBox(saved) {
  if (!confirm(`Delete box ${saved.code}? Anyone with the link will lose access.`)) return;
  try {
    await api("DELETE", `/api/boxes/${encodeURIComponent(saved.code)}`, {
      headers: { "X-Delete-Token": saved.deleteToken },
    });
    toast("Box deleted");
  } catch (error) {
    if (error.status !== 404) return toast(error.message);
    toast("Box was already gone");
  }
  forgetBox(saved.code);
  renderMine();
}

function boxRow(saved) {
  const row = clone("box-row");
  const remaining = secondsUntil(saved.expiresAt);
  const expired = remaining <= 0;

  const link = row.querySelector(".box-link");
  link.textContent = saved.code;
  link.href = `/b/${saved.code}`;
  row.querySelector(".box-meta").textContent = filesSummary(saved.fileCount, saved.size);
  row.querySelector(".pill").classList.toggle("is-expired", expired);
  row.querySelector(".box-expiry").textContent = expired ? "Expired" : `${formatRemaining(remaining)} left`;

  const actions = row.querySelector(".actions");
  if (expired) {
    actions.append(
      iconButton("icon-x", `Forget ${saved.code}`, () => {
        forgetBox(saved.code);
        renderMine();
      }),
    );
  } else {
    actions.append(
      iconButton("icon-copy", `Copy link to ${saved.code}`, () => copyText(saved.url)),
      iconButton("icon-trash", `Delete ${saved.code}`, () => deleteBox(saved), "danger"),
    );
  }
  return row;
}

function renderMine() {
  const boxes = loadBoxes();
  $("mine-list").replaceChildren(...boxes.map(boxRow));
  $("mine-list").hidden = boxes.length === 0;
  $("mine-empty").hidden = boxes.length > 0;
}

function showMine() {
  renderMine();
  show("mine", { title: "My boxes", nav: "mine" });
}

// ---------------------------------------------------------------------------
// Routing

async function route() {
  const path = location.pathname;
  const boxMatch = path.match(/^\/b\/([A-Za-z0-9]+)\/?$/);
  try {
    if (boxMatch) await showBox(boxMatch[1]);
    else if (path === "/mine") showMine();
    else showUpload();
  } catch (error) {
    show("gone", { title: "Error" });
    toast(error.message);
  }
}

function navigate(href) {
  if (href !== location.pathname) history.pushState(null, "", href);
  route();
  window.scrollTo(0, 0);
}

function onLinkClick(event) {
  const link = event.target.closest("a[data-link]");
  if (!link || event.defaultPrevented || event.button !== 0) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  navigate(link.getAttribute("href"));
}

// ---------------------------------------------------------------------------
// Startup

function bindUpload() {
  const dropzone = $("dropzone");
  const input = $("file-input");

  input.addEventListener("change", () => {
    addFiles(input.files);
    input.value = "";
  });

  dropzone.addEventListener("dragover", (event) => {
    event.preventDefault();
    dropzone.classList.add("is-over");
  });
  dropzone.addEventListener("dragleave", (event) => {
    if (!dropzone.contains(event.relatedTarget)) dropzone.classList.remove("is-over");
  });
  dropzone.addEventListener("drop", (event) => {
    event.preventDefault();
    dropzone.classList.remove("is-over");
    addFiles(event.dataTransfer.files);
  });
  // A file dropped next to the drop zone should not replace the page.
  window.addEventListener("dragover", (event) => event.preventDefault());
  window.addEventListener("drop", (event) => event.preventDefault());
  // Leaving the page would abandon an upload that may have been running for a long time.
  window.addEventListener("beforeunload", (event) => {
    if (state.uploading) event.preventDefault();
  });

  $("expiry-options").addEventListener("keydown", moveExpiry);
  $("upload-form").addEventListener("submit", submitUpload);
  $("done-copy").addEventListener("click", () => copyText($("done-url").value));
  $("done-url").addEventListener("focus", (event) => event.target.select());
}

function applyInfo(info) {
  state.info = info;
  state.expiry = info.default_expiry;
  $("limits-hint").textContent =
    `Up to ${plural(info.max_files, "file")} · ${formatBytes(info.max_upload_size)} total`;
  $("token-field").hidden = !info.upload_token_required;
  $("token-input").value = sessionStorage.getItem(TOKEN_KEY) ?? "";
  renderExpiryOptions();
}

async function start() {
  bindUpload();
  document.addEventListener("click", onLinkClick);
  window.addEventListener("popstate", route);
  try {
    applyInfo(await api("GET", "/api/info"));
  } catch (error) {
    show("upload", { nav: "upload" });
    setUploadError(`Could not reach the server: ${error.message}`);
    return;
  }
  await route();
}

start();
