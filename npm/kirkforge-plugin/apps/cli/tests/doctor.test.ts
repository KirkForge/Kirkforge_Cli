import { describe, it, expect } from "vitest";
import { doctor } from "@kirkforge/plugin";

describe("doctor", () => {
  it("returns a ToolCapabilityReport with expected keys", async () => {
    const report = await doctor();

    expect(report).toHaveProperty("eslint");
    expect(report).toHaveProperty("tsc");
    expect(report).toHaveProperty("ruff");
    expect(report).toHaveProperty("pyright");
    expect(report).toHaveProperty("bandit");
    expect(report).toHaveProperty("secdev");
    expect(report).toHaveProperty("languages");
  });

  it("each tool entry has available boolean and optional version", async () => {
    const report = await doctor();
    const toolEntries = [
      report.eslint,
      report.tsc,
      report.ruff,
      report.pyright,
      report.bandit,
      report.secdev,
    ];

    for (const entry of toolEntries) {
      expect(typeof entry.available).toBe("boolean");
      if (entry.version !== undefined) {
        expect(typeof entry.version).toBe("string");
      }
      expect(["internal", "external"]).toContain(entry.source);
    }
  });

  it("internal tools are always available with source internal", async () => {
    const report = await doctor();
    expect(report.secdev.available).toBe(true);
    expect(report.secdev.source).toBe("internal");
  });

  it("external tools have source external", async () => {
    const report = await doctor();
    expect(report.eslint.source).toBe("external");
    expect(report.tsc.source).toBe("external");
    expect(report.ruff.source).toBe("external");
    expect(report.pyright.source).toBe("external");
    expect(report.bandit.source).toBe("external");
  });

  it("languages is a non-empty array of strings", async () => {
    const report = await doctor();

    expect(Array.isArray(report.languages)).toBe(true);
    expect(report.languages.length).toBeGreaterThan(0);
    for (const lang of report.languages) {
      expect(typeof lang).toBe("string");
    }
  });

  it("serializes to valid JSON", async () => {
    const report = await doctor();
    const json = JSON.stringify(report);
    const parsed = JSON.parse(json);

    expect(parsed).toEqual(report);
  });
});
