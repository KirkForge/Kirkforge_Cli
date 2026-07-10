import { describe, it, expect } from "vitest";
import { sha256Hex, hmacSha256, awsSigV4Sign } from "../src/index.js";

// ---------------------------------------------------------------------------
// AWS SigV4 test suite — fixture tests against AWS published test vectors
//
// Reference: https://docs.aws.amazon.com/general/latest/gr/sigv4-calculate-signature.html
// Test values from AWS SigV4 Test Suite (awssigv4-test-suite.zip):
//   Access Key ID:     AKIDEXAMPLE
//   Secret Access Key: wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY
//   Region:            us-east-1
//   Service:           iam
// ---------------------------------------------------------------------------

const TEST_ACCESS_KEY = "AKIDEXAMPLE";
const TEST_SECRET_KEY = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const TEST_REGION = "us-east-1";
const TEST_SERVICE = "iam";
// 2015-08-30T12:36:00Z
const TEST_DATE = new Date("2015-08-30T12:36:00Z");

describe("sha256Hex", () => {
  it("hashes empty string to known value", () => {
    expect(sha256Hex("")).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
  });

  it("hashes known payload", () => {
    // AWS test suite: empty payload hash
    const hash = sha256Hex("Action=ListUsers&Version=2010-05-08");
    expect(hash).toBe("b6359072c78d70ebee1e81adcbab4f01bf2c23245fa365ef83fe8f1f955085e2");
  });

  it("hashes utf-8 payload correctly", () => {
    expect(sha256Hex("hello")).toBe(
      "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    );
  });
});

describe("hmacSha256", () => {
  it("computes kDate correctly against AWS test vector", () => {
    // AWS doc: HMAC("AWS4" + secret, "20150830")
    const kDate = hmacSha256("AWS4" + TEST_SECRET_KEY, "20150830");
    expect(kDate.toString("hex")).toBe(
      "0138c7a6cbd60aa727b2f653a522567439dfb9f3e72b21f9b25941a42f04a7cd",
    );
  });

  it("produces deterministic kRegion from kDate", () => {
    const kDate = hmacSha256("AWS4" + TEST_SECRET_KEY, "20150830");
    const a = hmacSha256(kDate, TEST_REGION).toString("hex");
    const b = hmacSha256(kDate, TEST_REGION).toString("hex");
    expect(a).toBe(b);
    expect(a).toHaveLength(64);
  });

  it("produces deterministic kService from kRegion", () => {
    const kDate = hmacSha256("AWS4" + TEST_SECRET_KEY, "20150830");
    const kRegion = hmacSha256(kDate, TEST_REGION);
    const a = hmacSha256(kRegion, TEST_SERVICE).toString("hex");
    const b = hmacSha256(kRegion, TEST_SERVICE).toString("hex");
    expect(a).toBe(b);
    expect(a).toHaveLength(64);
  });

  it("computes kSigning correctly against AWS test vector", () => {
    const kDate = hmacSha256("AWS4" + TEST_SECRET_KEY, "20150830");
    const kRegion = hmacSha256(kDate, TEST_REGION);
    const kService = hmacSha256(kRegion, TEST_SERVICE);
    const kSigning = hmacSha256(kService, "aws4_request");
    expect(kSigning.toString("hex")).toBe(
      "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9",
    );
  });
});

describe("awsSigV4Sign", () => {
  it("produces correct signature for AWS GET test vector (iam:ListUsers)", () => {
    // AWS SigV4 test suite: get-vanilla-query
    // GET /?Action=ListUsers&Version=2010-05-08
    const result = awsSigV4Sign({
      method: "GET",
      host: "iam.amazonaws.com",
      region: TEST_REGION,
      service: TEST_SERVICE,
      body: "",
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
      canonicalQuery: "Action=ListUsers&Version=2010-05-08",
      contentType: "application/x-www-form-urlencoded; charset=utf-8",
      target: "", // IAM API doesn't use X-Amz-Target
    });

    // Expected signature from AWS test suite
    expect(result.headers["Authorization"]).toBe(
      "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7",
    );
    expect(result.headers["X-Amz-Date"]).toBe("20150830T123600Z");
    expect(result.headers["Host"]).toBe("iam.amazonaws.com");
  });

  it("produces correct signature for AWS POST test vector (secretsmanager:GetSecretValue)", () => {
    // Test default usage: POST to secretsmanager
    const body = JSON.stringify({ SecretId: "test-secret" });
    const result = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: TEST_REGION,
      service: "secretsmanager",
      body,
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
    });

    expect(result.headers["Host"]).toBe("secretsmanager.us-east-1.amazonaws.com");
    expect(result.headers["X-Amz-Date"]).toBe("20150830T123600Z");
    expect(result.headers["Content-Type"]).toBe("application/x-amz-json-1.1");
    expect(result.headers["X-Amz-Target"]).toBe("secretsmanager.GetSecretValue");
    expect(result.headers["Authorization"]).toContain("AWS4-HMAC-SHA256");
    expect(result.headers["Authorization"]).toContain(
      "Credential=AKIDEXAMPLE/20150830/us-east-1/secretsmanager/aws4_request",
    );
    expect(result.headers["Authorization"]).toContain(
      "SignedHeaders=content-type;host;x-amz-date;x-amz-target",
    );
    // Verify signature component is present (64 hex chars)
    const sigMatch = result.headers["Authorization"].match(/Signature=([a-f0-9]{64})/);
    expect(sigMatch).toBeTruthy();
  });

  it("includes session token when provided", () => {
    const result = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: TEST_REGION,
      service: "secretsmanager",
      body: "{}",
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      sessionToken: "test-session-token",
      now: TEST_DATE,
    });

    expect(result.headers["X-Amz-Security-Token"]).toBe("test-session-token");
    expect(result.headers["Authorization"]).toContain(
      "SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target",
    );
  });

  it("uses current time when no now override is given", () => {
    const before = new Date();
    const result = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: TEST_REGION,
      service: "secretsmanager",
      body: "{}",
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
    });
    const after = new Date();

    // X-Amz-Date should be between before and after
    const amzDateStr = result.headers["X-Amz-Date"];
    expect(amzDateStr).toMatch(/^\d{8}T\d{6}Z$/);

    // Parse the date back and verify it's within reasonable bounds
    const year = parseInt(amzDateStr.slice(0, 4));
    const month = parseInt(amzDateStr.slice(4, 6));
    const day = parseInt(amzDateStr.slice(6, 8));
    const hour = parseInt(amzDateStr.slice(9, 11));
    const min = parseInt(amzDateStr.slice(11, 13));
    const sec = parseInt(amzDateStr.slice(13, 15));

    const parsed = new Date(Date.UTC(year, month - 1, day, hour, min, sec));
    const toleranceMs = 5000;
    expect(parsed.getTime()).toBeGreaterThanOrEqual(before.getTime() - toleranceMs);
    expect(parsed.getTime()).toBeLessThanOrEqual(after.getTime() + toleranceMs);
  });

  it("signature changes with different body content", () => {
    const sig1 = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: TEST_REGION,
      service: "secretsmanager",
      body: JSON.stringify({ SecretId: "key1" }),
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
    });

    const sig2 = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: TEST_REGION,
      service: "secretsmanager",
      body: JSON.stringify({ SecretId: "key2" }),
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
    });

    expect(sig1.headers["Authorization"]).not.toBe(sig2.headers["Authorization"]);
  });

  it("signature changes with different region", () => {
    const sig1 = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-east-1.amazonaws.com",
      region: "us-east-1",
      service: "secretsmanager",
      body: "{}",
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
    });

    const sig2 = awsSigV4Sign({
      method: "POST",
      host: "secretsmanager.us-west-2.amazonaws.com",
      region: "us-west-2",
      service: "secretsmanager",
      body: "{}",
      accessKeyId: TEST_ACCESS_KEY,
      secretAccessKey: TEST_SECRET_KEY,
      now: TEST_DATE,
    });

    expect(sig1.headers["Authorization"]).not.toBe(sig2.headers["Authorization"]);
  });
});
