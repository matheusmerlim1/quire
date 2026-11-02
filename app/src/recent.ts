// File ▸ Open Recent: the last documents opened or saved on this machine.
import { api } from "./api";

const KEY = "quire.recent";
const MAX = 10;

export function recentList(): string[] {
  try {
    const list = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(list) ? list.filter((p) => typeof p === "string") : [];
  } catch {
    return [];
  }
}

function write(list: string[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    // blocked storage: the menu still updates for this session
  }
}

/** The desktop app keeps only documents the user chose; the list follows, so the menu's items match it. */
function store(list: string[]): Promise<void> {
  write(list);
  return api
    .setRecent(list)
    .then(write)
    .catch(() => {});
}

export function addRecent(path: string) {
  store([path, ...recentList().filter((p) => p !== path)].slice(0, MAX));
}

export function forgetRecent(path: string) {
  store(recentList().filter((p) => p !== path));
}

export function clearRecent() {
  store([]);
}

/**
 * Fill the menu at startup. Versions up to 0.2.1 kept the recent lists only here; the desktop
 * app takes them over once, before any document is open, so they keep working.
 */
export async function syncRecent(notebooks: string[]) {
  await api.adoptRecent(recentList(), notebooks).catch(() => {});
  await store(recentList());
}
