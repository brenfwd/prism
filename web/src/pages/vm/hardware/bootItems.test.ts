import { describe, expect, test } from "bun:test";
import { bootIds, dropBootItem, moveBootItem, normalize, type BootItem } from "./bootItems";

const item = (id: string, enabled: boolean): BootItem => ({ id, label: id, sub: "", icon: "disk", enabled });
const ids = (l: BootItem[]) => l.map((x) => x.id).join("");

describe("boot order", () => {
  test("normalize puts enabled items first", () => {
    expect(ids(normalize([item("a", false), item("b", true), item("c", false), item("d", true)]))).toBe("bdac");
  });

  test("moving a disabled item above an enabled one enables it", () => {
    const next = moveBootItem([item("a", true), item("b", false)], 1, 0)!;
    expect(ids(next)).toBe("ba");
    expect(next.every((x) => x.enabled)).toBe(false);
    expect(bootIds(next)).toEqual(["b", "a"]);
  });

  test("moving a disabled item among disabled ones leaves it off", () => {
    const next = moveBootItem([item("a", true), item("b", false), item("c", false)], 2, 1)!;
    expect(ids(next)).toBe("acb");
    expect(bootIds(next)).toEqual(["a"]);
  });

  test("out-of-range and same-index moves are no-ops", () => {
    const list = [item("a", true), item("b", true)];
    expect(moveBootItem(list, 0, 0)).toBeNull();
    expect(moveBootItem(list, 0, -1)).toBeNull();
    expect(moveBootItem(list, 1, 2)).toBeNull();
  });

  test("dropping applies the same rule and does not mutate the input", () => {
    const list = [item("b", false), item("a", true)];
    expect(bootIds(dropBootItem(list, 0))).toEqual(["b", "a"]);
    expect(list[0].enabled).toBe(false);
    expect(bootIds(dropBootItem(list, -1))).toEqual(["a"]);
  });
});
