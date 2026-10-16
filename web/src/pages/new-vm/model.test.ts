import { describe, expect, test } from "bun:test";
import { GiB } from "../../lib/format";
import {
  baseName, blockers, capacity, defaultPreset, effectiveNet, hostnameOf, minDiskGib, nameError, networkRequest, resolveSize,
  suggestName,
} from "./model";

describe("names", () => {
  test("baseName takes the first word of the file name", () => {
    expect(baseName("ubuntu-24.04.img")).toBe("ubuntu");
    expect(baseName("debian-12-netinst.iso")).toBe("debian");
    expect(baseName("7.img")).toBe("vm");
    expect(baseName(undefined)).toBe("vm");
  });

  test("suggestName skips taken numbers", () => {
    expect(suggestName("web", new Set(["web-1", "web-2", "web-4"]))).toBe("web-3");
  });

  test("hostnameOf makes a DNS label", () => {
    expect(hostnameOf("My_VM.1")).toBe("my-vm-1");
    expect(hostnameOf("--")).toBe("vm");
    expect(hostnameOf("a".repeat(70))).toHaveLength(63);
  });

  test("nameError reports empty, invalid and taken names", () => {
    const taken = new Set(["db"]);
    expect(nameError("", taken)).toBe("Give it a name.");
    expect(nameError("a b", taken)).toContain("no spaces");
    expect(nameError("db", taken)).toContain("already");
    expect(nameError("web", taken)).toBeNull();
  });
});

describe("size", () => {
  test("defaultPreset needs 10 GiB free for Medium", () => {
    expect(defaultPreset(null)).toBe("small");
    expect(defaultPreset(9 * GiB)).toBe("small");
    expect(defaultPreset(10 * GiB)).toBe("medium");
  });

  test("minDiskGib rounds an image up to whole GiB", () => {
    expect(minDiskGib("image", 2.2 * GiB)).toBe(3);
    expect(minDiskGib("iso", 5 * GiB)).toBe(1);
  });

  test("resolveSize raises the disk to the minimum", () => {
    const custom = { vcpus: 3, mem: 512, disk: 2 };
    expect(resolveSize("small", custom, 20)).toEqual({ vcpus: 1, mem: 1024, disk: 20 });
    expect(resolveSize("custom", custom, 1)).toEqual(custom);
  });
});

describe("capacity", () => {
  const size = { vcpus: 1, mem: 1024, disk: 10 };
  test("flags memory over and tight", () => {
    expect(capacity(size, 512 * 1024 * 1024, undefined).memOver).toBe(true);
    const tight = capacity(size, 1200 * 1024 * 1024, undefined);
    expect(tight.memOver).toBe(false);
    expect(tight.memTight).toBe(true);
    expect(capacity(size, 4 * GiB, undefined).memTight).toBe(false);
    expect(capacity(size, null, undefined).memOver).toBe(false);
  });

  test("flags a disk larger than the pool's free space", () => {
    expect(capacity(size, null, 5 * GiB).diskOver).toBe(true);
    expect(capacity(size, null, 10 * GiB).diskOver).toBe(false);
    expect(capacity(size, null, undefined).diskOver).toBe(false);
  });
});

describe("submission", () => {
  const ok = { nameError: null, source: "image" as const, hasSource: true, badKey: false, poolReady: true };
  test("blockers lists problems in order", () => {
    expect(blockers(ok)).toEqual([]);
    expect(blockers({ ...ok, nameError: "bad", hasSource: false, badKey: true, poolReady: false })).toEqual([
      "bad", "Pick a cloud image.", "Fix the SSH key.", "Create a storage pool first.",
    ]);
    expect(blockers({ ...ok, source: "blank", hasSource: false })).toEqual([]);
    expect(blockers({ ...ok, source: "iso", hasSource: false })).toEqual(["Pick an ISO."]);
  });

  test("a Prism network falls back to NAT when none exists", () => {
    expect(effectiveNet("network", undefined)).toBe("user");
    expect(effectiveNet("network", "n1")).toBe("network");
    expect(effectiveNet("bridge", undefined)).toBe("bridge");
  });

  test("networkRequest builds the API shape", () => {
    expect(networkRequest("network", "n1", "br0")).toEqual({ type: "network", network: "n1" });
    expect(networkRequest("bridge", undefined, "br1")).toEqual({ type: "bridge", bridge: "br1" });
    expect(networkRequest("user", "n1", "br0")).toEqual({ type: "user" });
  });
});
