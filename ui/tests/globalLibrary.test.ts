import { describe, expect, it } from "vitest";
import { getLibraryAssetUrl } from "../src/ipc";
import type { AssetSummary, LibraryItem } from "../src/ipc/schemas";
import {
  extractUniqueTags,
  filterLibraryItems,
  formatLibraryImportName,
  getMediaTypeGroup,
} from "../src/lib/globalLibrary";
import { buildLibraryRows } from "../src/lib/library";

describe("globalLibrary pure helpers", () => {
  const sampleItems: LibraryItem[] = [
    {
      id: "lib-1",
      name: "sunset.png",
      tags: ["nature", "photo"],
      media_type: "image/png",
      size: 1024,
      hash: "h1",
      added_at: "2026-10-30T10:00:00Z",
      removed: false,
    },
    {
      id: "lib-2",
      name: "notes.txt",
      tags: ["work"],
      media_type: "text/plain",
      size: 512,
      hash: "h2",
      added_at: "2026-10-30T11:00:00Z",
      removed: false,
    },
    {
      id: "lib-3",
      name: "demo.mp4",
      tags: ["work", "video"],
      media_type: "video/mp4",
      size: 2048,
      hash: "h3",
      added_at: "2026-10-30T12:00:00Z",
      removed: false,
    },
    {
      id: "lib-4",
      name: "sound.mp3",
      tags: [],
      media_type: "audio/mpeg",
      size: 4096,
      hash: "h4",
      added_at: "2026-10-30T13:00:00Z",
      removed: false,
    },
    {
      id: "lib-5",
      name: "document.pdf",
      tags: ["doc"],
      media_type: "application/pdf",
      size: 8192,
      hash: "h5",
      added_at: "2026-10-30T14:00:00Z",
      removed: false,
    },
  ];

  it("classifies media types correctly", () => {
    expect(getMediaTypeGroup("image/jpeg")).toBe("image");
    expect(getMediaTypeGroup("image/png")).toBe("image");
    expect(getMediaTypeGroup("text/plain")).toBe("text");
    expect(getMediaTypeGroup("text/markdown")).toBe("text");
    expect(getMediaTypeGroup("video/webm")).toBe("video");
    expect(getMediaTypeGroup("audio/wav")).toBe("audio");
    expect(getMediaTypeGroup("application/pdf")).toBe("other");
    expect(getMediaTypeGroup("unknown")).toBe("other");
  });

  it("filters library items by search, tag, and media type", () => {
    // Search by name
    const searchResult = filterLibraryItems(sampleItems, "sunset", null, "all");
    expect(searchResult.map((i) => i.id)).toEqual(["lib-1"]);

    // Filter by tag
    const tagResult = filterLibraryItems(sampleItems, "", "work", "all");
    expect(tagResult.map((i) => i.id)).toEqual(["lib-2", "lib-3"]);

    // Filter by media type
    const mediaResult = filterLibraryItems(sampleItems, "", null, "text");
    expect(mediaResult.map((i) => i.id)).toEqual(["lib-2"]);

    // Combined filter
    const combined = filterLibraryItems(sampleItems, "demo", "work", "video");
    expect(combined.map((i) => i.id)).toEqual(["lib-3"]);

    // No matches
    const noMatch = filterLibraryItems(sampleItems, "nonexistent", null, "all");
    expect(noMatch).toHaveLength(0);
  });

  it("extracts sorted unique tags from items", () => {
    const tags = extractUniqueTags(sampleItems);
    expect(tags).toEqual(["doc", "nature", "photo", "video", "work"]);
  });

  it("formats import names for single and multiple items", () => {
    expect(formatLibraryImportName([sampleItems[0]!])).toBe("sunset.png");
    expect(formatLibraryImportName(sampleItems)).toBe("Library import (5)");
  });

  it("constructs global library asset URIs with non-colliding library prefix", () => {
    expect(getLibraryAssetUrl("asset-123")).toBe("mera-asset://localhost/library/asset-123");
  });

  it("builds asset rows with provenance from library or removed notice", () => {
    const libraryNames = new Map([["lib-1", "sunset.png"]]);
    const assets: AssetSummary[] = [
      {
        id: "ast-1",
        media_type: "image/png",
        size: 1024,
        origin: "step-1",
        library_item: "lib-1",
      },
      {
        id: "ast-2",
        media_type: "image/png",
        size: 1024,
        origin: "step-2",
        library_item: "lib-deleted",
      },
      {
        id: "ast-3",
        media_type: "text/plain",
        size: 256,
        origin: "step-3",
      },
    ];

    const rows = buildLibraryRows(
      {
        collections: [],
        workflows: [],
        runs: [],
        assets,
        libraryNames,
      },
      {
        collectionDetail: (c, v) => `${c} slots · v${v}`,
        workflowDetail: (c, v) => `${c} steps · v${v}`,
        runState: (s) => s,
      },
    );

    const row1 = rows.find((r) => r.id === "ast-1");
    expect(row1?.provenance).toBe("From library: sunset.png");
    expect(row1?.detail).toBe("Image · 1 KB");

    const row2 = rows.find((r) => r.id === "ast-2");
    expect(row2?.provenance).toBe("Removed from library");

    const row3 = rows.find((r) => r.id === "ast-3");
    expect(row3?.provenance).toBeUndefined();
  });
});
