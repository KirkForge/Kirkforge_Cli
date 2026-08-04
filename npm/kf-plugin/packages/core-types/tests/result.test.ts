import { describe, it, expect } from "vitest";
import {
  ok,
  err,
  isOk,
  isErr,
  map,
  mapErr,
  unwrap,
  unwrapOrElse,
  expect as expectResult,
} from "../src/result.js";

describe("Result<T, E>", () => {
  it("ok creates success result", () => {
    const r = ok(42);
    expect(r.ok).toBe(true);
    expect(r.value).toBe(42);
  });

  it("err creates error result", () => {
    const r = err(new Error("boom"));
    expect(r.ok).toBe(false);
    expect(r.error.message).toBe("boom");
  });

  it("isOk type guard", () => {
    const r = ok("hello");
    if (isOk(r)) expect(r.value).toBe("hello");
    else throw new Error("should be ok");
  });

  it("isErr type guard", () => {
    const r = err("fail");
    if (isErr(r)) expect(r.error).toBe("fail");
    else throw new Error("should be err");
  });

  it("map transforms success values", () => {
    const r = map(ok(10), (v) => v * 2);
    expect(r).toEqual(ok(20));
  });

  it("map passes through errors", () => {
    const r = map(err("nope"), (v) => (v as unknown as number) * 2);
    expect(r).toEqual(err("nope"));
  });

  it("mapErr transforms errors", () => {
    expect(mapErr(err("old"), (e) => `new: ${e}`)).toEqual(err("new: old"));
  });

  it("unwrap returns value or fallback", () => {
    expect(unwrap(ok(5), 0)).toBe(5);
    expect(unwrap(err("x"), 0)).toBe(0);
  });

  it("unwrapOrElse lazy fallback", () => {
    expect(unwrapOrElse(ok(5), () => 0)).toBe(5);
    expect(unwrapOrElse(err("x"), (e) => e.length)).toBe(1);
  });

  it("expect throws on error", () => {
    expect(() => expectResult(ok(5), "never")).not.toThrow();
    expect(() => expectResult(err("fail"), "msg")).toThrow("msg");
  });
});
