// Auth-aware healthcheck for Docker HEALTHCHECK
// Reads HEALTH_PORT and HEALTH_API_KEY from the environment
const http = require("http");
const port = process.env.HEALTH_PORT || 9090;
const opts = { hostname: "localhost", port, path: "/healthz" };
if (process.env.HEALTH_API_KEY) {
  opts.headers = { Authorization: "Bearer " + process.env.HEALTH_API_KEY };
}
http
  .get(opts, (r) => {
    process.exit(r.statusCode === 200 ? 0 : 1);
  })
  .on("error", () => process.exit(1));
