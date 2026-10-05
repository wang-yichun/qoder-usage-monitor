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

console.log("token prefix:", token.slice(0, 3), "| length:", token.length, "| has whitespace:", /\s/.test(token));

const targets = [
  { name: "api2.qoder.sh /v1/models", url: "https://api2.qoder.sh/v1/models" },
  { name: "api2.qoder.sh /api/v1/models", url: "https://api2.qoder.sh/api/v1/models" },
  { name: "openapi.qoder.sh /api/v1/models", url: "https://openapi.qoder.sh/api/v1/models" },
  { name: "openapi.qoder.sh /sash/api/v1/ai-conversations/credits-summary", url: "https://openapi.qoder.sh/sash/api/v1/ai-conversations/credits-summary" },
];

for (const t of targets) {
  try {
    const res = await fetch(t.url, { headers: { Authorization: `Bearer ${token}`, Accept: "application/json" }, redirect: "error", signal: AbortSignal.timeout(15000) });
    const body = await res.text();
    console.log(`[${res.status}] ${t.name}`);
    console.log("  ", body.slice(0, 200).replace(/\s+/g, " "));
  } catch (err) {
    console.log(`[ERR] ${t.name}: ${err.name} ${err.message}`);
  }
}
