import type { AuditSink, AuditSinkConfig } from "./audit.js";
import { FileAuditSink } from "./audit-sinks/file.js";
import { HttpAuditSink } from "./audit-sinks/http.js";
import { MemoryAuditSink } from "./audit-sinks/memory.js";
import { SyslogAuditSink } from "./audit-sinks/syslog.js";

/**
 * Create an audit sink from config. In enterprise mode, "memory" is not
 * accepted and will throw — use validateEnterpriseAudit() instead.
 */
export function createAuditSink(config: AuditSinkConfig): AuditSink {
  switch (config.type) {
    case "file":
      if (!config.filePath) throw new Error("File audit sink requires filePath");
      return new FileAuditSink({
        filePath: config.filePath,
        flushInterval: config.flushInterval,
        maxFileSizeBytes: config.maxFileSizeBytes,
        maxRotatedFiles: config.maxRotatedFiles,
      });
    case "http":
      if (!config.httpUrl) throw new Error("HTTP audit sink requires httpUrl");
      return new HttpAuditSink({
        url: config.httpUrl,
        headers: config.httpHeaders,
        flushInterval: config.flushInterval,
      });
    case "syslog":
      return new SyslogAuditSink({
        transport: config.syslogTransport,
        host: config.syslogHost,
        port: config.syslogPort,
        facility: config.syslogFacility,
        appName: config.syslogAppName,
        flushInterval: config.flushInterval,
        tls: config.syslogTls,
      });
    case "memory":
      return new MemoryAuditSink();
    default:
      throw new Error(`Unknown audit sink type: ${config.type}`);
  }
}
