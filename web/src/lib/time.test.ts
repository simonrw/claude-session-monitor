import { describe, it, expect, vi, afterEach } from "vitest";
import { relativeTime } from "./time";

describe("relativeTime", () => {
  afterEach(() => vi.useRealTimers());

  function at(now: string) {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(now));
  }

  it("just now for < 60s", () => {
    at("2026-04-26T10:01:00Z");
    expect(relativeTime("2026-04-26T10:00:30Z")).toBe("just now");
  });

  it("minutes ago", () => {
    at("2026-04-26T10:05:00Z");
    expect(relativeTime("2026-04-26T10:03:00Z")).toBe("2m ago");
  });

  it("hours ago", () => {
    at("2026-04-26T13:00:00Z");
    expect(relativeTime("2026-04-26T10:00:00Z")).toBe("3h ago");
  });

  it("days ago", () => {
    at("2026-04-28T10:00:00Z");
    expect(relativeTime("2026-04-26T10:00:00Z")).toBe("2d ago");
  });

  it("1 minute boundary", () => {
    at("2026-04-26T10:01:00Z");
    expect(relativeTime("2026-04-26T10:00:00Z")).toBe("1m ago");
  });

  it("1 hour boundary", () => {
    at("2026-04-26T11:00:00Z");
    expect(relativeTime("2026-04-26T10:00:00Z")).toBe("1h ago");
  });
});
