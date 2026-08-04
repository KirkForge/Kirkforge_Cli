import type { ServerResponse } from "node:http";
import { KirkForgeError, toErrorResponse } from "@kirkforge/core-errors";

/** Send a 401 Unauthorized response with a structured error body. */
export function sendUnauthorized(res: ServerResponse, reason: string): void {
  const errResp = toErrorResponse(new KirkForgeError("UNAUTHORIZED", reason));
  res.writeHead(401, {
    "Content-Type": "application/json",
    "WWW-Authenticate": 'Bearer realm="kirkforge"',
  });
  res.end(JSON.stringify(errResp));
}

/** Send a 403 Forbidden response with a structured error body. */
export function sendForbidden(res: ServerResponse, reason: string): void {
  const errResp = toErrorResponse(new KirkForgeError("FORBIDDEN", reason));
  res.writeHead(403, { "Content-Type": "application/json" });
  res.end(JSON.stringify(errResp));
}

/** Send a structured error response using the core-errors catalog. */
export function sendError(res: ServerResponse, error: Error, requestId?: string): void {
  const errResp = toErrorResponse(error, requestId);
  const status = errResp.error.status;
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(errResp));
}
