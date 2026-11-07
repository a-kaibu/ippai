import { describe, expect, it } from "vite-plus/test";
import { stateOf, type Saved } from "./offline";

const rec = (status: Saved["status"]): Saved => ({ trees: ["prefix32", "g"], status });

describe("stateOf", () => {
  it("is complete only when saved to the end and every chunk is still here", () => {
    expect(stateOf(rec("done"), false, 10, 10)).toEqual({ status: "done", complete: true });
    // (the browser's storage cleared in part)
    expect(stateOf(rec("done"), false, 9, 10)).toEqual({ status: "done", complete: false });
    // every chunk read while playing, never saved: the app's files may be missing
    expect(stateOf(undefined, false, 10, 10)).toEqual({ status: "none", complete: false });
  });
  it("tells a stopped or failed save from a finished one", () => {
    expect(stateOf(rec("stopped"), false, 10, 10).complete).toBe(false);
    expect(stateOf(rec("failed"), false, 3, 10)).toEqual({ status: "failed", complete: false });
  });
  it("reads a save left running by a closed page as stopped", () => {
    expect(stateOf(rec("saving"), true, 3, 10)).toEqual({ status: "saving", complete: false });
    expect(stateOf(rec("saving"), false, 3, 10)).toEqual({ status: "stopped", complete: false });
    expect(stateOf(rec("saving"), false, 10, 10).complete).toBe(false);
  });
});
