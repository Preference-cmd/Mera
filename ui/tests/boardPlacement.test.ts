import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  calculatePlacementCenter,
  calculatePlacementPosition,
  COLLECTION_CARD_SIZE,
} from "../src/lib/boardPlacement";
import { findNonOverlappingPosition } from "../src/lib/library";
import { useSettingsStore } from "../src/store/settingsStore";
import { useUIStore } from "../src/store/uiStore";

describe("boardPlacement helpers", () => {
  beforeEach(() => {
    useSettingsStore.setState({ sidebarCollapsed: false });
    useUIStore.setState({ sessionPanelOpen: false, canvasViewports: {} });
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1440 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 900 });
  });

  afterEach(() => vi.restoreAllMocks());

  test("returns explicit drop point when supplied and centers the card on it", () => {
    expect(calculatePlacementCenter("p1", { x: 500, y: 400 })).toEqual({ x: 500, y: 400 });
    expect(calculatePlacementPosition("p1", COLLECTION_CARD_SIZE, { x: 500, y: 400 })).toEqual({
      x: 350,
      y: 300,
    });
  });

  test("computes visible canvas center from stored viewport when drop point is omitted", () => {
    useUIStore.setState({
      sessionPanelOpen: true,
      canvasViewports: { p1: { x: -600, y: -300, zoom: 1 } },
    });
    expect(calculatePlacementCenter("p1")).toEqual({ x: 986, y: 718 });
  });

  test("staggers placement away from already-occupied board coordinates", () => {
    const size = { width: 240, height: 160 };
    const first = findNonOverlappingPosition({ x: 500, y: 400 }, size, []);
    expect(first).toEqual({ x: 380, y: 320 });

    const second = findNonOverlappingPosition({ x: 500, y: 400 }, size, [
      { position: first, size },
    ]);
    expect(second).toEqual({ x: 460, y: 400 });
  });
});
