import { describe, it, expect, vi, afterEach } from "vitest";
import { Logger } from "../src/index.js";

describe("Logger", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("writes info logs to stdout by default", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "info", format: "json" });
    logger.info("test message");
    expect(stdoutWrite).toHaveBeenCalled();
    const written = stdoutWrite.mock.calls.map((c) => c[0]).join("");
    const parsed = JSON.parse(written.trim());
    expect(parsed.message).toBe("test message");
    expect(parsed.level).toBe("info");
    stderrWrite.mockRestore();
    stdoutWrite.mockRestore();
  });

  it("writes info logs to stderr when stream is stderr", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "info", format: "json", stream: "stderr" });
    logger.info("test message");
    expect(stderrWrite).toHaveBeenCalled();
    const written = stderrWrite.mock.calls.map((c) => c[0]).join("");
    const parsed = JSON.parse(written.trim());
    expect(parsed.message).toBe("test message");
    expect(parsed.level).toBe("info");
    expect(stdoutWrite).not.toHaveBeenCalled();
    stdoutWrite.mockRestore();
    stderrWrite.mockRestore();
  });

  it("writes error logs to stderr regardless of stream setting", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "info", format: "json", stream: "stdout" });
    logger.error("error message");
    expect(stderrWrite).toHaveBeenCalled();
    const written = stderrWrite.mock.calls.map((c) => c[0]).join("");
    const parsed = JSON.parse(written.trim());
    expect(parsed.message).toBe("error message");
    expect(parsed.level).toBe("error");
    expect(stdoutWrite).not.toHaveBeenCalled();
    stdoutWrite.mockRestore();
    stderrWrite.mockRestore();
  });

  it("writes all levels to stderr when stream is stderr", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "trace", format: "json", stream: "stderr" });
    logger.trace("trace msg");
    logger.debug("debug msg");
    logger.info("info msg");
    logger.warn("warn msg");
    logger.error("error msg");
    expect(stdoutWrite).not.toHaveBeenCalled();
    expect(stderrWrite).toHaveBeenCalledTimes(5);
    stdoutWrite.mockRestore();
    stderrWrite.mockRestore();
  });

  it("human format routes all output to stderr when stream is stderr", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "info", format: "human", stream: "stderr" });
    logger.info("human test");
    expect(stderrWrite).toHaveBeenCalled();
    const written = stderrWrite.mock.calls.map((c) => c[0]).join("");
    expect(written).toContain("INFO");
    expect(written).toContain("human test");
    expect(stdoutWrite).not.toHaveBeenCalled();
    stdoutWrite.mockRestore();
    stderrWrite.mockRestore();
  });

  it("child logger inherits parent transport (stderr)", () => {
    const stdoutWrite = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const stderrWrite = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    const logger = new Logger({ level: "info", format: "json", stream: "stderr" });
    const child = logger.child({});
    child.info("child message");
    expect(stderrWrite).toHaveBeenCalled();
    const written = stderrWrite.mock.calls.map((c) => c[0]).join("");
    const parsed = JSON.parse(written.trim());
    expect(parsed.message).toBe("child message");
    expect(stdoutWrite).not.toHaveBeenCalled();
    stdoutWrite.mockRestore();
    stderrWrite.mockRestore();
  });
});
