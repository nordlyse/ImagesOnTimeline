const stage = document.querySelector("#stage");
const strip = document.querySelector("#strip");
const empty = document.querySelector("#empty");
const summary = document.querySelector("#summary");
const dateText = document.querySelector("#date-text");
const datePlace = document.querySelector("#date-place");
const dateStart = document.querySelector("#date-start");
const dateEnd = document.querySelector("#date-end");
const scrub = document.querySelector("#scrub");

const REACH = 5;

let pictures = [];
let index = 0;
let scannedAt = null;
let wheelBudget = 0;
let drag = null;

document.querySelector("#step-back").addEventListener("click", () => step(-1));
document.querySelector("#step-forward").addEventListener("click", () => step(1));
scrub.addEventListener("input", () => select(Number(scrub.value)));

stage.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || event.target.closest("button, input, a, video")) return;
  drag = { x: event.clientX, index };
  stage.setPointerCapture(event.pointerId);
});

stage.addEventListener("pointermove", (event) => {
  if (!drag) return;
  const jumps = Math.round((drag.x - event.clientX) / 70);
  select(drag.index + jumps);
});

stage.addEventListener("pointerup", () => {
  drag = null;
});

stage.addEventListener("pointercancel", () => {
  drag = null;
});

stage.addEventListener("wheel", (event) => {
  if (pictures.length === 0) return;
  event.preventDefault();
  const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
  wheelBudget += delta;
  const jumps = Math.trunc(wheelBudget / 60);
  if (jumps !== 0) {
    wheelBudget -= jumps * 60;
    step(jumps);
  }
}, { passive: false });

window.addEventListener("keydown", (event) => {
  if (document.querySelector("#library").hidden === false) return;
  if (event.key === "ArrowRight" || event.key === "ArrowDown") step(1);
  else if (event.key === "ArrowLeft" || event.key === "ArrowUp") step(-1);
  else if (event.key === "Home") select(0);
  else if (event.key === "End") select(pictures.length - 1);
  else if (event.key === "PageDown") step(8);
  else if (event.key === "PageUp") step(-8);
  else return;
  event.preventDefault();
});

bindLibrary();
paint();

function step(amount) {
  select(index + amount);
}

function select(next) {
  if (pictures.length === 0) {
    index = 0;
    paint();
    return;
  }
  const clamped = Math.min(pictures.length - 1, Math.max(0, next));
  if (clamped === index && strip.childElementCount > 0) {
    scrub.value = String(index);
    return;
  }
  index = clamped;
  paint();
}

function paint() {
  strip.replaceChildren();
  scrub.disabled = pictures.length === 0;
  if (pictures.length === 0) {
    empty.hidden = false;
    empty.textContent = scannedAt
      ? "This folder has no pictures or videos."
      : "Choose a folder to lay its pictures and videos out on the timeline.";
    dateText.textContent = "";
    datePlace.textContent = "";
    datePlace.hidden = true;
    dateStart.textContent = "";
    dateEnd.textContent = "";
    scrub.max = "0";
    scrub.value = "0";
    return;
  }
  empty.hidden = true;
  const start = Math.max(0, index - REACH);
  const end = Math.min(pictures.length - 1, index + REACH);
  for (let position = start; position <= end; position += 1) {
    const picture = pictures[position];
    const distance = Math.abs(position - index);
    const current = position === index;
    const frame = document.createElement(current && picture.video ? "div" : "button");
    if (frame.tagName === "BUTTON") frame.type = "button";
    frame.className = "frame";
    frame.style.setProperty("--distance", String(distance));
    if (current) frame.setAttribute("aria-current", "true");
    frame.setAttribute("aria-label", pictureLabel(picture));
    const media = mediaElement(picture, current);
    const shot = document.createElement("div");
    shot.className = "shot";
    if (current) {
      shot.append(rainbowLayer("rainbow-glow"), rainbowLayer("rainbow-ring"));
    }
    shot.append(media);
    const caption = document.createElement("span");
    caption.textContent = formatDay(picture.captured_at);
    frame.append(shot, caption);
    if (frame.tagName === "BUTTON") frame.addEventListener("click", () => select(position));
    strip.append(frame);
  }
  scrub.max = String(pictures.length - 1);
  scrub.value = String(index);
  showWhen(pictures[index]);
  dateStart.textContent = formatDay(pictures[0].captured_at);
  dateEnd.textContent = formatDay(pictures[pictures.length - 1].captured_at);
  requestAnimationFrame(alignStrip);
}

function mediaElement(picture, current) {
  if (!picture.video) {
    const image = document.createElement("img");
    image.alt = "";
    image.addEventListener("load", alignStrip);
    image.src = `/api/pictures/${picture.id}/file`;
    return image;
  }
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.preload = current ? "auto" : "metadata";
  video.controls = current;
  if (current) video.autoplay = true;
  video.addEventListener("loadedmetadata", alignStrip);
  video.src = `/api/pictures/${picture.id}/file`;
  return video;
}

function rainbowLayer(className) {
  const layer = document.createElement("div");
  layer.className = className;
  const spin = document.createElement("div");
  spin.className = "rainbow-spin";
  layer.append(spin);
  return layer;
}

function alignStrip() {
  const current = strip.querySelector(".frame[aria-current='true']");
  strip.style.transform = "";
  if (!current) return;
  const stageBox = stage.getBoundingClientRect();
  const box = current.getBoundingClientRect();
  const delta = (stageBox.left + stageBox.width / 2) - (box.left + box.width / 2);
  strip.style.transform = `translateX(${delta}px)`;
}

window.addEventListener("resize", alignStrip);

function showWhen(picture) {
  dateText.textContent = formatDate(picture.captured_at);
  const place = typeof picture.place === "string" ? picture.place.trim() : "";
  datePlace.textContent = place;
  datePlace.hidden = place.length === 0;
}

function pictureLabel(picture) {
  const when = formatDate(picture.captured_at);
  const place = typeof picture.place === "string" ? picture.place.trim() : "";
  if (!place) return `${picture.name}, ${when}`;
  return `${picture.name}, ${when}, ${place}`;
}

function formatDate(value) {
  const when = new Date(value);
  if (Number.isNaN(when.getTime())) return value;
  return when.toLocaleString("en-GB", {
    day: "numeric",
    month: "long",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDay(value) {
  const when = new Date(value);
  if (Number.isNaN(when.getTime())) return value;
  return when.toLocaleDateString("en-GB", {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

function bindLibrary() {
  const dialog = document.querySelector("#library");
  const openButton = document.querySelector("#library-open");
  const closeButton = document.querySelector("#library-close");
  const upButton = document.querySelector("#library-up");
  const useButton = document.querySelector("#library-use");
  const rescanButton = document.querySelector("#library-rescan");
  const pathLabel = document.querySelector("#library-path");
  const list = document.querySelector("#library-list");
  const status = document.querySelector("#library-status");
  let currentLabel = "";
  let parentLabel = null;
  let busy = false;

  openButton.addEventListener("click", () => openDialog());
  closeButton.addEventListener("click", () => {
    dialog.hidden = true;
    stage.focus();
  });
  upButton.addEventListener("click", () => {
    if (parentLabel != null) showFolders(parentLabel);
  });
  useButton.addEventListener("click", () => showFolder(currentLabel, false));
  rescanButton.addEventListener("click", () => showFolder(currentLabel, true));
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.hidden = true;
  });

  loadStatus();

  async function loadStatus() {
    const response = await fetch("/api/library");
    if (!response.ok) return;
    const body = await response.json();
    scannedAt = body.scanned_at || null;
    if (body.label) {
      currentLabel = body.label;
      openButton.textContent = folderName(body.label);
      await loadPictures();
    }
  }

  async function openDialog() {
    dialog.hidden = false;
    await showFolders(currentLabel);
  }

  async function showFolders(label) {
    status.textContent = "";
    const params = new URLSearchParams();
    if (label) params.set("path", label);
    const response = await fetch(`/api/library/folders?${params}`);
    if (!response.ok) {
      status.textContent = await response.text();
      return;
    }
    const view = await response.json();
    currentLabel = view.label;
    parentLabel = view.parent;
    pathLabel.textContent = view.label;
    upButton.disabled = view.parent == null;
    rescanButton.hidden = !view.scanned_at;
    if (view.scanned_at) status.textContent = `Scanned ${formatScan(view.scanned_at)}`;
    list.replaceChildren();
    if (view.entries.length === 0) {
      const note = document.createElement("p");
      note.className = "library-note";
      note.textContent = "No folders inside this one.";
      list.append(note);
      return;
    }
    for (const entry of view.entries) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "folder-choice";
      button.textContent = entry.name;
      button.addEventListener("click", () => showFolders(entry.label));
      list.append(button);
    }
  }

  async function showFolder(label, rescan) {
    if (busy) return;
    busy = true;
    useButton.disabled = true;
    rescanButton.disabled = true;
    status.textContent = rescan ? "Scanning folder" : "Opening folder";
    const response = await fetch("/api/library", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: label, rescan }),
    });
    busy = false;
    useButton.disabled = false;
    rescanButton.disabled = false;
    if (!response.ok) {
      status.textContent = await response.text();
      return;
    }
    const body = await response.json();
    currentLabel = body.label;
    scannedAt = body.scanned_at || null;
    openButton.textContent = folderName(body.label);
    status.textContent = pictureSummary(body.files);
    summary.textContent = status.textContent;
    dialog.hidden = true;
    await loadPictures();
    stage.focus();
  }
}

async function loadPictures() {
  const response = await fetch("/api/pictures");
  if (!response.ok) {
    summary.textContent = "Pictures could not be read.";
    return;
  }
  pictures = await response.json();
  index = 0;
  summary.textContent = pictureSummary(pictures.length);
  paint();
}

function pictureSummary(count) {
  const text = count === 1 ? "1 picture" : `${count} pictures`;
  if (!scannedAt) return text;
  return `${text}, scanned ${formatScan(scannedAt)}`;
}

function formatScan(value) {
  const when = new Date(value);
  if (Number.isNaN(when.getTime())) return value;
  return when.toLocaleString("en-GB", {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function folderName(label) {
  const parts = label.split("/").filter(Boolean);
  return parts.length === 0 ? label : parts[parts.length - 1];
}
