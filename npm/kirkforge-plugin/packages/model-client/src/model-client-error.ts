export type ModelClientErrorCode =
  | "NETWORK_ERROR"
  | "TIMEOUT"
  | "AUTH_ERROR"
  | "RATE_LIMIT"
  | "API_ERROR"
  | "PARSE_ERROR";

export class ModelClientError extends Error {
  public readonly retryAfterMs?: number;
  constructor(
    message: string,
    public readonly code: ModelClientErrorCode,
    public readonly statusCode?: number,
    retryAfterMs?: number,
  ) {
    super(message);
    this.name = "ModelClientError";
    this.retryAfterMs = retryAfterMs;
  }
  static network(msg: string) {
    return new ModelClientError(msg, "NETWORK_ERROR");
  }
  static timeout(ms: number) {
    return new ModelClientError(`Request timed out after ${ms}ms`, "TIMEOUT");
  }
  static auth(msg: string) {
    return new ModelClientError(msg, "AUTH_ERROR", 401);
  }
  static rateLimit(msg: string, retryAfterMs?: number) {
    return new ModelClientError(msg, "RATE_LIMIT", 429, retryAfterMs);
  }
  static api(status: number, msg: string) {
    return new ModelClientError(msg, "API_ERROR", status);
  }
  static parse(msg: string) {
    return new ModelClientError(msg, "PARSE_ERROR");
  }
}
