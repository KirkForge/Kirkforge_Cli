import { readFileSync, existsSync } from "node:fs";
import type { SecretsProvider } from "../manager.js";

export interface GcpSecretsConfig {
  /** GCP project ID. */
  projectId: string;
  /** Service account access token or path to credentials file. */
  accessToken?: string;
  credentialsFile?: string;
}

/**
 * Resolves secrets from Google Cloud Secret Manager. Access tokens are
 * minted on demand from a service-account credentials file via self-signed
 * JWT (RS256), then cached until near-expiry.
 */
export class GcpSecretsProvider implements SecretsProvider {
  readonly name = "gcp-secret-manager";
  private config: GcpSecretsConfig;
  private cachedToken: string | null = null;
  private tokenExpiry: number = 0;

  constructor(config: GcpSecretsConfig) {
    this.config = config;
  }

  private async resolveAccessToken(): Promise<string | null> {
    // Use explicit access token if provided
    if (this.config.accessToken) return this.config.accessToken;

    // Check cached token
    if (this.cachedToken && Date.now() < this.tokenExpiry) {
      return this.cachedToken;
    }

    // Try loading from credentials file
    const credsPath = this.config.credentialsFile ?? process.env.GOOGLE_APPLICATION_CREDENTIALS;
    if (credsPath && existsSync(credsPath)) {
      try {
        const credsRaw = readFileSync(credsPath, "utf-8");
        const creds = JSON.parse(credsRaw) as {
          client_email?: string;
          private_key?: string;
          token_uri?: string;
        };

        if (creds.client_email && creds.private_key && creds.token_uri) {
          const token = await this.mintJwtAccessToken(
            creds.client_email,
            creds.private_key,
            creds.token_uri,
          );
          if (token) {
            this.cachedToken = token.access_token;
            this.tokenExpiry = Date.now() + (token.expires_in ?? 3600) * 1000 - 60000;
            return this.cachedToken;
          }
        }
      } catch {
        // Credentials file parse failed — try next method
      }
    }

    return null;
  }

  private async mintJwtAccessToken(
    clientEmail: string,
    privateKey: string,
    tokenUri: string,
  ): Promise<{ access_token: string; expires_in: number } | null> {
    try {
      // Create a self-signed JWT for service account authentication
      const header = { alg: "RS256", typ: "JWT" };
      const now = Math.floor(Date.now() / 1000);
      const claims = {
        iss: clientEmail,
        scope: "https://www.googleapis.com/auth/cloud-platform",
        aud: tokenUri,
        exp: now + 3600,
        iat: now,
      };

      const headerB64 = Buffer.from(JSON.stringify(header)).toString("base64url");
      const claimsB64 = Buffer.from(JSON.stringify(claims)).toString("base64url");
      const unsigned = `${headerB64}.${claimsB64}`;

      const { createSign } = await import("node:crypto");
      const sign = createSign("RSA-SHA256");
      sign.update(unsigned);
      const signature = sign.sign(privateKey, "base64url");
      const jwt = `${unsigned}.${signature}`;

      const res = await fetch(tokenUri, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=${jwt}`,
        signal: AbortSignal.timeout(10000),
      });

      if (!res.ok) return null;
      return (await res.json()) as { access_token: string; expires_in: number };
    } catch {
      return null;
    }
  }

  async get(key: string): Promise<string | null> {
    try {
      const accessToken = await this.resolveAccessToken();
      if (!accessToken) return null;

      const projectId = this.config.projectId;
      const secretName = `projects/${projectId}/secrets/${key}/versions/latest`;
      const url = `https://secretmanager.googleapis.com/v1/${secretName}:access`;

      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        Authorization: `Bearer ${accessToken}`,
      };

      const res = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(5000),
      });

      if (!res.ok) return null;
      const data = (await res.json()) as {
        payload?: { data?: string };
      };

      if (data.payload?.data) {
        // GCP returns base64-encoded secret data
        return Buffer.from(data.payload.data, "base64").toString("utf-8");
      }
      return null;
    } catch {
      return null;
    }
  }
}
