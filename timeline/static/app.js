const stage = document.querySelector("#stage");
const strip = document.querySelector("#strip");
const empty = document.querySelector("#empty");
const summary = document.querySelector("#summary");
const dateLabel = document.querySelector("#date-label");
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
  if (event.button !== 0 || event.target.closest("button, input, a")) return;
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
      ? "This folder has no pictures."
      : "Choose a folder to lay its pictures out on the timeline.";
    dateLabel.textContent = "";
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
    const button = document.createElement("button");
    button.type = "button";
    button.className = "frame";
    button.style.setProperty("--distance", String(distance));
    if (position === index) button.setAttribute("aria-current", "true");
    button.setAttribute("aria-label", `${picture.name}, ${formatDate(picture.captured_at)}`);
    const image = document.createElement("img");
    image.alt = "";
    image.addEventListener("load", alignStrip);
    image.src = `/api/pictures/${picture.id}/file`;
    const shot = document.createElement("div");
    shot.className = "shot";
    if (position === index) {
      shot.append(rainbowLayer("rainbow-glow"), rainbowLayer("rainbow-ring"));
    }
    shot.append(image);
    const caption = document.createElement("span");
    caption.textContent = formatDay(picture.captured_at);
    button.append(shot, caption);
    button.addEventListener("click", () => select(position));
    strip.append(button);
  }
  scrub.max = String(pictures.length - 1);
  scrub.value = String(index);
  dateLabel.textContent = formatDate(pictures[index].captured_at);
  dateStart.textContent = formatDay(pictures[0].captured_at);
  dateEnd.textContent = formatDay(pictures[pictures.length - 1].captured_at);
  requestAnimationFrame(alignStrip);
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
  useButton.addEventListener("click", () => showFolder(currentLabel));
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
    if (view.scanned_at) status.textContent = `Scanned ${formatDay(view.scanned_at)}`;
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

  async function showFolder(label) {
    if (busy) return;
    busy = true;
    useButton.disabled = true;
    status.textContent = "Opening folder";
    const response = await fetch("/api/library", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: label }),
    });
    busy = false;
    useButton.disabled = false;
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
  return `${text}, scanned ${formatDay(scannedAt)}`;
}

function folderName(label) {
  const parts = label.split("/").filter(Boolean);
  return parts.length === 0 ? label : parts[parts.length - 1];
}
