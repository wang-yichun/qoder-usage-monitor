import fs from "node:fs";

// read PAT from .env (bare token or KEY=VALUE)
let token = process.env.QODER_PERSONAL_ACCESS_TOKEN;
if (!token) {
  const text = fs.readFileSync("D:/Projects/qoder-usage-monitor/.env", "utf8");
  for (const line of text.split(/\r?\n/)) {
    const m = /^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)\s*$/.exec(line);
    if (m && m[1] === "QODER_PERSONAL_ACCESS_TOKEN") { token = m[2].replace(/^["']|["']$/g, ""); break; }
  }
  if (!token) token = text.split(/\r?\n/).map((l) => l.trim()).find((l) => l && !l.startsWith("#"));
}

const targets = [
  { name: "cn sash me/usage (Bearer)", url: "https://openapi.qoder.com.cn/sash/api/v2/me/usage", headers: { Authorization: `Bearer ${token}` } },
  { name: "global sash me/usage (X-Api-Key)", url: "https://openapi.qoder.sh/sash/api/v2/me/usage", headers: { "X-Api-Key": token } },
  { name: "global sash me/usage (Bearer + Api-Key suffix)", url: "https://openapi.qoder.sh/sash/api/v2/me/usage", headers: { Authorization: `Bearer ${token}`, "X-Api-Key": token } },
  { name: "api.qoder.com /v1/organizations", url: "https://api.qoder.com/v1/organizations", headers: { Authorization: `Bearer ${token}`, Accept: "application/json" } },
  { name: "api.qoder.com /v1/me", url: "https://api.qoder.com/v1/me", headers: { Authorization: `Bearer ${token}`, Accept: "application/json" } },
  { name: "openapi.qoder.sh /v1/me", url: "https://openapi.qoder.sh/v1/me", headers: { Authorization: `Bearer ${token}`, Accept: "application/json" } },
];

for (const t of targets) {
  try {
    const res = await fetch(t.url, { headers: t.headers, redirect: "error", signal: AbortSignal.timeout(15000) });
    const body = await res.text();
    console.log(`[${res.status}] ${t.name}`);
    console.log("  ", body.slice(0, 220).replace(/\s+/g, " "));
  } catch (err) {
    console.log(`[ERR] ${t.name}: ${err.name} ${err.message}`);
  }
}
