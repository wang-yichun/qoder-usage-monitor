import fs from "node:fs";

let token = process.env.QODER_PERSONAL_ACCESS_TOKEN;
if (!token) {
  const text = fs.readFileSync("D:/Projects/qoder-usage-monitor/.env", "utf8");
  for (const line of text.split(/\r?\n/)) {
    const m = /^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)\s*$/.exec(line);
    if (m && m[1] === "QODER_PERSONAL_ACCESS_TOKEN") { token = m[2].replace(/^["']|["']$/g, ""); break; }
  }
  if (!token) token = text.split(/\r?\n/).map((l) => l.trim()).find((l) => l && !l.startsWith("#"));
}

const H = { Authorization: `Bearer ${token}`, Accept: "application/json" };

async function probe(name, url, init = {}) {
  try {
    const res = await fetch(url, { headers: H, redirect: "error", signal: AbortSignal.timeout(15000), ...init });
    const body = await res.text();
    console.log(`[${res.status}] ${name}`);
    console.log("  ", body.slice(0, 260).replace(/\s+/g, " "));
  } catch (err) {
    console.log(`[ERR] ${name}: ${err.name} ${err.message}`);
  }
}

await probe("openapi /api/v2/user/plan", "https://openapi.qoder.sh/api/v2/user/plan");
await probe("openapi /api/v1/partner_plans", "https://openapi.qoder.sh/api/v1/me/partner_plans");
await probe(
  "serviceToken/exchange with PAT",
  "https://openapi.qoder.sh/api/v1/serviceToken/exchange",
  { method: "POST", headers: { ...H, "Content-Type": "application/json" }, body: JSON.stringify({ grant_type: "client_credentials", audience: "qoder" }) }
);
await probe("openapi /api/v1/me", "https://openapi.qoder.sh/api/v1/me");
await probe("cn openapi /api/v2/user/plan", "https://openapi.qoder.com.cn/api/v2/user/plan");
