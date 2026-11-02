import { beforeEach, describe, expect, it, vi } from "vitest";

// the desktop app's answers: it keeps only documents the user chose
const backend = vi.hoisted(() => ({
  chosen: new Set<string>(),
  adopted: null as null | { documents: string[]; notebooks: string[] },
}));

vi.mock("./api", () => ({
  api: {
    setRecent: (paths: string[]) => Promise.resolve(paths.filter((p) => backend.chosen.has(p))),
    adoptRecent: (documents: string[], notebooks: string[]) => {
      backend.adopted = { documents, notebooks };
      for (const p of documents) backend.chosen.add(p);
      return Promise.resolve(null);
    },
  },
}));

import { addRecent, recentList, syncRecent } from "./recent";

const settle = () => new Promise((r) => setTimeout(r, 0));

describe("recent documents", () => {
  beforeEach(() => {
    localStorage.clear();
    backend.chosen.clear();
    backend.adopted = null;
  });

  it("keeps the list the desktop app kept, so the menu's items match it", async () => {
    backend.chosen.add("/docs/Beam.quire");
    localStorage.setItem("quire.recent", JSON.stringify(["/etc/planted.quire"]));
    addRecent("/docs/Beam.quire");
    await settle();
    expect(recentList()).toEqual(["/docs/Beam.quire"]);
  });

  it("hands an older version's lists to the desktop app before filling the menu", async () => {
    localStorage.setItem("quire.recent", JSON.stringify(["/docs/Old.quire"]));
    await syncRecent(["/books/Physics"]);
    expect(backend.adopted).toEqual({ documents: ["/docs/Old.quire"], notebooks: ["/books/Physics"] });
    expect(recentList()).toEqual(["/docs/Old.quire"]);
  });
});
