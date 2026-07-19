import { listen, Event } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface WindowInfo {
  pid: number;
  window_id: number;
  app_name: string;
  window_title: string;
  icon_base64: string;
  is_active: boolean;
}

interface ShowOverlayPayload {
  windows: WindowInfo[];
  selected: number;
}

const app = document.getElementById("app")!;
const cardsContainer = document.getElementById("cards")!;
const titleEl = document.getElementById("title")!;

function showApp() {
  app.style.display = "block";
}

function hideApp() {
  app.style.display = "none";
}

function renderCards(windows: WindowInfo[]) {
  cardsContainer.innerHTML = "";
  windows.forEach((w, i) => {
    const card = document.createElement("div");
    card.className = "card";
    card.dataset.index = String(i);

    const iconUrl = w.icon_base64
      ? `data:image/png;base64,${w.icon_base64}`
      : "";

    card.innerHTML = `
      <img class="app-icon" src="${iconUrl}" alt="${w.app_name}" />
      <span class="app-name">${w.app_name}</span>
    `;

    card.addEventListener("click", () => {
      invoke("activate_window", { pid: w.pid });
    });

    cardsContainer.appendChild(card);
  });
}

function updateSelection(index: number) {
  document.querySelectorAll(".card").forEach((el) => {
    el.classList.remove("card--selected");
  });
  const card = document.querySelector(`.card[data-index="${index}"]`);
  if (card) {
    card.classList.add("card--selected");
    const windowTitle = card.querySelector(".app-name")?.textContent || "";
    titleEl.textContent = windowTitle;
  }
}

listen<ShowOverlayPayload>("show-overlay", (event: Event<ShowOverlayPayload>) => {
  const payload = event.payload;
  renderCards(payload.windows);
  updateSelection(payload.selected);
  showApp();
});

listen<{ selected: number }>("update-selection", (event: Event<{ selected: number }>) => {
  updateSelection(event.payload.selected);
});

listen("hide-overlay", () => {
  hideApp();
});

console.log("[oh-my-tab] Frontend initialized");
