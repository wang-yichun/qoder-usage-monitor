// 解密本地 Qoder OAuth 凭据并验证用量 API 是否可用
// 绝不打印 token / refreshToken / DPAPI key 的值，只显示长度与前缀
import { readFileSync } from "node:fs";
import { createDecipheriv } from "node:crypto";
import { join } from "node:path";
import { homedir } from "node:os";
import { execFileSync } from "node:child_process";

const APP_DIR = join(homedir(), "AppData", "Roaming", "com.qoder.app.stable");
const ENDPOINTS = [
  ["cn", "https://openapi.qoder.com.cn"],
  ["global", "https://openapi.qoder.sh"],
];
const TIMEOUT_MS = 15000;

function getDpapiKey() {
  const ps = [
    "Add-Type -AssemblyName System.Security",
    "$enc = [Convert]::FromBase64String((Get-Content \"$env:APPDATA\\com.qoder.app.stable\\Local State\" -Raw | ConvertFrom-Json).os_crypt.encrypted_key)",
    "$key = [Security.Cryptography.ProtectedData]::Unprotect($enc[5..($enc.Length-1)], $null, \"CurrentUser\")",
    "[BitConverter]::ToString($key) -replace \"-\",\"\"",
  ].join("; ");
  const out = execFileSync("powershell.exe", ["-NoProfile", "-Command", ps], { encoding: "utf8" });
  const hex = out.replace(/[^0-9a-fA-F]/g, "");
  if (hex.length !== 64) throw new Error(`DPAPI key length ${hex.length / 2} != 32`);
  return Buffer.from(hex, "hex");
}

function decrypt(blob, key) {
  const nonce = blob.subarray(3, 15);
  const tag = blob.subarray(blob.length - 16);
  const data = blob.subarray(15, blob.length - 16);
  const d = createDecipheriv("aes-256-gcm", key, nonce);
  d.setAuthTag(tag);
  return Buffer.concat([d.update(data), d.final()]);
}

function loadAuth() {
  const key = getDpapiKey();
  const plain = decrypt(readFileSync(join(APP_DIR, "auth.v1.dat")), key);
  return JSON.parse(plain.toString("utf8"));
}

function fmtDur(ms) {
  const neg = ms < 0;
  ms = Math.abs(ms);
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  const s = Math.floor((ms % 60000) / 1000);
  return `${neg ? "-" : ""}${h}h${String(m).padStart(2, "0")}m${String(s).padStart(2, "0")}s`;
}

function redact(value, keyName = "") {
  if (typeof value === "string") {
    if (/token|secret|credential|apikey|api_key/i.test(keyName)) {
      return `<redacted len=${value.length}>`;
    }
    return value;
  }
  if (Array.isArray(value)) return value.map((v) => redact(v));
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) out[k] = redact(v, k);
    return out;
  }
  return value;
}

async function callApi(baseUrl, token, path) {
  const res = await fetch(`${baseUrl}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(TIMEOUT_MS),
    redirect: "manual",
  });
  const text = await res.text();
  let json = null;
  try { json = JSON.parse(text); } catch {}
  return { status: res.status, json, text: json ? undefined : text.slice(0, 300) };
}

const auth = loadAuth();
const now = Date.now();
console.log("=== auth.v1.dat 凭据状态 ===");
console.log(`user: ${auth.user?.name} <${auth.user?.email}>`);
console.log(`access token: <len=${auth.token?.length} prefix=${String(auth.token).slice(0, 2)}>`);
const exp = auth.expiresAt ? new Date(auth.expiresAt) : null;
if (exp && !Number.isNaN(exp.getTime())) {
  const left = exp.getTime() - now;
  console.log(`access 过期时间: ${auth.expiresAt}（${left > 0 ? "剩余 " + fmtDur(left) : "已过期 " + fmtDur(left)}）`);
}
const rexp = auth.refreshTokenExpiresAt ? new Date(auth.refreshTokenExpiresAt) : null;
if (rexp && !Number.isNaN(rexp.getTime())) {
  const left = rexp.getTime() - now;
  console.log(`refresh 过期时间: ${auth.refreshTokenExpiresAt}（${left > 0 ? "剩余 " + fmtDur(left) : "已过期 " + fmtDur(left)}）`);
}

console.log("\n=== 用量 API 测试 ===");
for (const [region, base] of ENDPOINTS) {
  for (const path of ["/sash/api/v2/me/usage", "/api/v2/user/plan"]) {
    process.stdout.write(`[${region}] ${path} ... `);
    try {
      const r = await callApi(base, auth.token, path);
      if (r.json) {
        console.log(`HTTP ${r.status}`);
        console.log(JSON.stringify(redact(r.json), null, 2));
      } else {
        console.log(`HTTP ${r.status} 非 JSON: ${r.text}`);
      }
    } catch (err) {
      console.log(`失败: ${err.message}`);
    }
    console.log("");
  }
}
