import { describe, it, test, expect } from "vitest";
import { Greeter, makeGreeter } from "../src/greeter";

describe("Greeter", () => {
  it("greets casually by default", () => {
    expect(makeGreeter("Ada").greet()).toBe("Hello, Ada!");
  });

  test("supports formal greeting", () => {
    const g = new Greeter({ name: "Grace", formal: true });
    expect(g.greet()).toBe("Good day, Grace!");
  });

  it("says goodbye", () => {
    expect(makeGreeter("Linus").goodbye()).toBe("Bye, Linus!");
  });
});
