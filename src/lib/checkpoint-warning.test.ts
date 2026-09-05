import { describe, expect, it } from "vitest";
import { CheckpointWarningThrottle } from "./checkpoint-warning";

describe("checkpoint warning throttle", () => {
  it("reports once per document and error code within the interval", () => {
    const throttle = new CheckpointWarningThrottle(5 * 60 * 1000);

    expect(throttle.shouldShow("document-a", "disk_full", 1_000)).toBe(true);
    expect(throttle.shouldShow("document-a", "disk_full", 2_000)).toBe(false);
    expect(throttle.shouldShow("document-a", "permission_denied", 2_000)).toBe(true);
    expect(throttle.shouldShow("document-a", "disk_full", 3_000)).toBe(false);
    expect(throttle.shouldShow("document-b", "disk_full", 2_000)).toBe(true);
    expect(throttle.shouldShow("document-a", "permission_denied", 302_001)).toBe(true);
  });

  it("does not suppress indefinitely after the wall clock moves backwards", () => {
    const throttle = new CheckpointWarningThrottle(5 * 60 * 1000);
    expect(throttle.shouldShow("document-a", "disk_full", 10_000)).toBe(true);
    expect(throttle.shouldShow("document-a", "disk_full", 5_000)).toBe(true);
  });

  it("allows the next failure after a successful checkpoint resets the document", () => {
    const throttle = new CheckpointWarningThrottle(5 * 60 * 1000);
    expect(throttle.shouldShow("document-a", "disk_full", 1_000)).toBe(true);
    throttle.reset("document-a");
    expect(throttle.shouldShow("document-a", "disk_full", 2_000)).toBe(true);
  });
});
