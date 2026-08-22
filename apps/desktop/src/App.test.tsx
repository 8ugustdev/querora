/* SPDX-License-Identifier: Apache-2.0 */
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "./App";

vi.mock("./lib/tauri-bridge", () => ({
  ping: () => Promise.reject(new Error("no tauri")),
  listSessions: () => Promise.resolve([]),
  onAgentEvent: () => () => {},
  sessionMessages: () => Promise.resolve([]),
  getAgentPrefs: () =>
    Promise.resolve({ defaultAgent: "pi", piModel: "zai/glm-5.3", piEffort: "medium", claudeModel: "" }),
}));

describe("App shell", () => {
  it("renders the Querora brand, chat surface and agent picker", async () => {
    render(<App />);
    expect(screen.getByText("Querora")).toBeTruthy();
    expect(screen.getByText("Chat")).toBeTruthy();
    expect(screen.getByText("Settings")).toBeTruthy();
    expect(screen.getByPlaceholderText("Ask your data…")).toBeTruthy();
    // agent selector offers all three drivers
    const select = screen.getByDisplayValue("pi");
    expect(select).toBeTruthy();
    // bridge falls back gracefully outside Tauri
    expect(await screen.findByText(/unavailable/)).toBeTruthy();
  });
});
