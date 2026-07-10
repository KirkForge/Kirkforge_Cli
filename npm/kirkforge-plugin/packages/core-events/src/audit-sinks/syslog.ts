import { createSocket } from "node:dgram";
import { connect as tlsConnect, type TLSSocket } from "node:tls";
import { createConnection as netCreateConnection, type Socket } from "node:net";
import { readFileSync } from "node:fs";
import type { AuditEvent, AuditOutcome, AuditSink } from "../audit.js";
import { initialHash, chainHashOf } from "../audit-chain-hash.js";

export interface SyslogAuditSinkConfig {
  /** Syslog transport: "udp", "tcp", or "tls". Default: "udp".
   *  "tls" uses RFC 5425 TLS-protected syslog for enterprise SIEM integration. */
  transport?: "udp" | "tcp" | "tls";
  /** Remote syslog host. Default: "localhost". */
  host?: string;
  /** Remote syslog port. Default: 514. */
  port?: number;
  /** Syslog facility code (0–23). Default: 1 (user-level). */
  facility?: number;
  /** Application name in syslog messages. Default: "kirkforge". */
  appName?: string;
  /** Buffer size before forcing flush. Default: 50. */
  flushInterval?: number;
  /** TLS options for "tls" transport. Required when transport is "tls". */
  tls?: {
    /** Path to CA certificate for server verification. */
    ca?: string;
    /** Path to client certificate for mTLS. */
    cert?: string;
    /** Path to client private key for mTLS. */
    key?: string;
    /** Whether to reject unauthorized server certificates. Default: true. */
    rejectUnauthorized?: boolean;
    /** Server name for SNI. Default: host. */
    servername?: string;
  };
  /** HMAC key for chain integrity. When set, chain hashes use HMAC-SHA256.
   *  Also read from KIRKFORGE_AUDIT_KEY env var. */
  hmacKey?: string;
}

/**
 * Syslog audit sink that sends audit events in CEF (Common Event Format)
 * over UDP or TCP syslog. Designed for SIEM integration (Splunk, Elastic,
 * Sentinel, etc.).
 *
 * Each audit event is formatted as a structured syslog message with:
 *   - PRI: facility * 8 + severity
 *   - HEADER: timestamp, hostname, appName
 *   - CEF body: action, outcome, actor, tenant, reason, chainHash
 *
 * For deny and error outcomes, severity is WARNING (4).
 * For success outcomes, severity is INFORMATIONAL (6).
 * For skipped outcomes, severity is DEBUG (7).
 */
export class SyslogAuditSink implements AuditSink {
  readonly name = "syslog";
  private transport: "udp" | "tcp" | "tls";
  private host: string;
  private port: number;
  private facility: number;
  private appName: string;
  private buffer: AuditEvent[] = [];
  private flushSize: number;
  private lastHash: string;
  private hmacKey?: string;
  private socket: ReturnType<typeof import("node:dgram").createSocket> | null = null;
  private tlsSocket: TLSSocket | Socket | null = null;
  private tlsConfig: SyslogAuditSinkConfig["tls"];
  private reconnecting = false;

  constructor(config: SyslogAuditSinkConfig = {}) {
    this.transport = config.transport ?? "udp";
    this.host = config.host ?? "localhost";
    this.port = config.port ?? (config.transport === "tls" ? 6514 : 514);
    this.facility = config.facility ?? 1; // user-level
    this.appName = config.appName ?? "kirkforge";
    this.flushSize = config.flushInterval ?? 50;
    this.tlsConfig = config.tls;
    this.hmacKey = config.hmacKey ?? process.env["KIRKFORGE_AUDIT_KEY"];
    this.lastHash = initialHash(this.hmacKey);
  }

  async write(event: AuditEvent): Promise<boolean> {
    this.buffer.push(event);
    if (this.buffer.length >= this.flushSize) {
      return this.flush();
    }
    return true;
  }

  async flush(): Promise<boolean> {
    if (this.buffer.length === 0) return true;
    const events = this.buffer.splice(0);
    let allOk = true;
    for (const event of events) {
      const chainHash = chainHashOf(this.lastHash, event, this.hmacKey);
      this.lastHash = chainHash;
      const message = this.formatMessage({ ...event, chainHash });
      const ok = await this.send(message);
      if (!ok) allOk = false;
    }
    return allOk;
  }

  async close(): Promise<void> {
    await this.flush();
    if (this.tlsSocket) {
      try {
        this.tlsSocket.destroy();
      } catch (_e) {
        // best-effort
      }
      this.tlsSocket = null;
    }
    if (this.socket) {
      try {
        this.socket.close();
      } catch (_e) {
        // best-effort
      }
      this.socket = null;
    }
  }

  private severityForOutcome(outcome: AuditOutcome): number {
    switch (outcome) {
      case "deny":
      case "error":
        return 4; // warning
      case "success":
        return 6; // informational
      case "skipped":
        return 7; // debug
      default:
        return 6;
    }
  }

  private formatMessage(event: AuditEvent): string {
    const severity = this.severityForOutcome(event.outcome);
    const pri = this.facility * 8 + severity;
    const ts = event.timestamp.replace("T", " ").replace("Z", "");
    const hostname =
      typeof process !== "undefined" && process.env?.HOSTNAME ? process.env.HOSTNAME : "kirkforge";

    // CEF format: CEF:Version|Device Vendor|Device Product|Device Version|Signature ID|Name|Severity|Extensions
    const severityLabel = severity <= 3 ? "High" : severity <= 5 ? "Medium" : "Low";
    const extensions = [
      `actor=${event.actorId}`,
      `tenant=${event.tenantId}`,
      `outcome=${event.outcome}`,
      `chainHash=${event.chainHash}`,
      event.policyHash ? `policyHash=${event.policyHash}` : "",
      event.traceId ? `traceId=${event.traceId}` : "",
    ]
      .filter(Boolean)
      .join(" ");

    return `<${pri}>${ts} ${hostname} ${this.appName} audit: CEF:0|KirkForge|Audit|1.0|${event.action}|${event.reason}|${severityLabel}|${extensions}`;
  }

  private async send(message: string): Promise<boolean> {
    const data = Buffer.from(message + "\n", "utf-8");
    if (this.transport === "udp") {
      return this.sendUdp(data);
    }
    if (this.transport === "tls") {
      return this.sendTls(data);
    }
    return this.sendTcp(data);
  }

  private async sendUdp(data: Buffer): Promise<boolean> {
    return new Promise((resolve) => {
      try {
        const sock = createSocket("udp4");
        sock.send(data, this.port, this.host, (err: Error | null) => {
          sock.close();
          resolve(!err);
        });
      } catch (_e) {
        resolve(false);
      }
    });
  }

  private async sendTcp(data: Buffer): Promise<boolean> {
    return new Promise((resolve) => {
      try {
        const socket = netCreateConnection({ host: this.host, port: this.port }, () => {
          socket.write(data, (err) => {
            if (err) {
              socket.destroy();
              resolve(false);
            } else {
              socket.end(() => {
                resolve(true);
              });
            }
          });
        });
        socket.on("error", () => resolve(false));
        socket.setTimeout(5000, () => {
          socket.destroy();
          resolve(false);
        });
      } catch (_e) {
        resolve(false);
      }
    });
  }

  /**
   * Send audit event over TLS-protected syslog connection (RFC 5425).
   * Establishes a TLS connection to the syslog server, transmits the message,
   * and closes the connection. Supports mutual TLS (mTLS) when cert/key are
   * provided in the TLS config.
   */
  private async sendTls(data: Buffer): Promise<boolean> {
    return new Promise((resolve) => {
      try {
        // readFileSync is already imported at module scope

        const tlsOptions: import("node:tls").ConnectionOptions = {
          host: this.host,
          port: this.port,
          rejectUnauthorized: this.tlsConfig?.rejectUnauthorized ?? true,
          servername: this.tlsConfig?.servername ?? this.host,
        };

        if (this.tlsConfig?.ca) {
          tlsOptions.ca = readFileSync(this.tlsConfig.ca, "utf-8");
        }
        if (this.tlsConfig?.cert) {
          tlsOptions.cert = readFileSync(this.tlsConfig.cert, "utf-8");
        }
        if (this.tlsConfig?.key) {
          tlsOptions.key = readFileSync(this.tlsConfig.key, "utf-8");
        }

        const socket = tlsConnect(tlsOptions, () => {
          if (!socket.authorized && (this.tlsConfig?.rejectUnauthorized ?? true)) {
            socket.destroy();
            resolve(false);
            return;
          }
          socket.write(data, (err) => {
            if (err) {
              socket.destroy();
              resolve(false);
            } else {
              socket.end(() => {
                resolve(true);
              });
            }
          });
        });

        socket.on("error", () => resolve(false));
        socket.setTimeout(5000, () => {
          socket.destroy();
          resolve(false);
        });
      } catch (_e) {
        resolve(false);
      }
    });
  }
}
