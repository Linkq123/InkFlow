import { describe, expect, it } from "vitest";
import { SaveSuspensions } from "./save-suspensions";

describe("save suspensions", () => {
  it("keeps saves suspended until every overlapping mutation releases its own lease", () => {
    const suspensions = new SaveSuspensions();
    const finishRename = suspensions.acquire(["alpha", "alpha", "beta"]);
    const finishClose = suspensions.acquire(["alpha"]);
    finishRename();
    expect(suspensions.has("alpha")).toBe(true);
    expect(suspensions.has("beta")).toBe(false);
    finishClose();
    expect(suspensions.has("alpha")).toBe(false);

    const finishNextRename = suspensions.acquire(["alpha"]);
    finishRename();
    finishClose();
    expect(suspensions.has("alpha")).toBe(true);
    finishNextRename();
    expect(suspensions.has("alpha")).toBe(false);
  });
});
