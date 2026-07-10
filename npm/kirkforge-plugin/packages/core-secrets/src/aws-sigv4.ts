import { createHash, createHmac } from "node:crypto";

export function sha256Hex(data: string): string {
  return createHash("sha256").update(data).digest("hex");
}

export function hmacSha256(key: Buffer | string, data: string): Buffer {
  return createHmac("sha256", key).update(data).digest();
}

export interface SigV4SignOptions {
  method: string;
  host: string;
  region: string;
  service: string;
  body: string;
  accessKeyId: string;
  secretAccessKey: string;
  sessionToken?: string;
  /** ISO-8601 timestamp override for deterministic testing. */
  now?: Date;
  /** Canonical URI override (default "/"). */
  canonicalUri?: string;
  /** Canonical query override (default ""). */
  canonicalQuery?: string;
  /** Content type override (default "application/x-amz-json-1.1"). */
  contentType?: string;
  /** X-Amz-Target override (default "secretsmanager.GetSecretValue"). */
  target?: string;
}

/**
 * Signs an AWS request with SigV4. Returns the Authorization header set
 * needed for `application/x-amz-json-1.1` AWS Secrets Manager calls.
 */
export function awsSigV4Sign(opts: SigV4SignOptions): { headers: Record<string, string> } {
  const now = opts.now ?? new Date();
  const amzDate =
    now
      .toISOString()
      .replace(/[:-]|\.\d{3}/g, "")
      .slice(0, 15) + "Z";
  const dateStamp = amzDate.slice(0, 8);

  const canonicalUri = opts.canonicalUri ?? "/";
  const canonicalQuery = opts.canonicalQuery ?? "";
  const payloadHash = sha256Hex(opts.body);
  const contentType = opts.contentType ?? "application/x-amz-json-1.1";
  const target = opts.target ?? "secretsmanager.GetSecretValue";

  const canonicalHeaders = [
    `content-type:${contentType}`,
    `host:${opts.host}`,
    `x-amz-date:${amzDate}`,
  ];
  if (target) {
    canonicalHeaders.push(`x-amz-target:${target}`);
  }
  if (opts.sessionToken) {
    canonicalHeaders.push(`x-amz-security-token:${opts.sessionToken}`);
  }
  canonicalHeaders.sort();
  const canonicalHeadersStr = canonicalHeaders.join("\n") + "\n";
  const signedHeaders = canonicalHeaders.map((h) => h.split(":")[0]!).join(";");

  const canonicalRequest = [
    opts.method,
    canonicalUri,
    canonicalQuery,
    canonicalHeadersStr,
    signedHeaders,
    payloadHash,
  ].join("\n");

  const algorithm = "AWS4-HMAC-SHA256";
  const credentialScope = `${dateStamp}/${opts.region}/${opts.service}/aws4_request`;
  const stringToSign = [algorithm, amzDate, credentialScope, sha256Hex(canonicalRequest)].join(
    "\n",
  );

  const kDate = hmacSha256("AWS4" + opts.secretAccessKey, dateStamp);
  const kRegion = hmacSha256(kDate, opts.region);
  const kService = hmacSha256(kRegion, opts.service);
  const kSigning = hmacSha256(kService, "aws4_request");
  const signature = hmacSha256(kSigning, stringToSign).toString("hex");

  const authorization = `${algorithm} Credential=${opts.accessKeyId}/${credentialScope}, SignedHeaders=${signedHeaders}, Signature=${signature}`;

  const headers: Record<string, string> = {
    "Content-Type": contentType,
    Host: opts.host,
    "X-Amz-Date": amzDate,
    Authorization: authorization,
  };
  if (target) {
    headers["X-Amz-Target"] = target;
  }
  if (opts.sessionToken) {
    headers["X-Amz-Security-Token"] = opts.sessionToken;
  }

  return { headers };
}
