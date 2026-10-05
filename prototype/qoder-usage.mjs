#!/usr/bin/env node
// Qoder Usage Monitor - CLI prototype
// Polls GET {base}/sash/api/v2/me/usage with Bearer auth and renders quota status.
// Credentials: auto-decrypt the desktop app's auth.v1.dat (Electron safeStorage),
// with --token / QODER_PERSONAL_ACCESS_TOKEN as fallbacks.
// Zero dependencies, requires Node.js 18+.

import process from "node:process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";
import { execFileSync } from "node:child_process";
import { createDecipheriv } from "node:crypto";

// load .env from prototype/ or project root (first hit wins); never overrides real env vars
{
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  for (const dir of [scriptDir, join(scriptDir, "..")]) {
    let text;
    try {
      text = readFileSync(join(dir, ".env"), "utf8");
    } catch {
      continue;
    }
    let hasPair = false;
    for (const line of text.split(/\r?\n/)) {
      const m = /^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)\s*$/.exec(line);
      if (m) {
        hasPair = true;
        if (process.env[m[1]] === undefined) {
          process.env[m[1]] = m[2].replace(/^["']|["']$/g, "");
        }
      }
    }
    if (!hasPair) {
      // bare token on a single line
      const bare = text.split(/\r?\n/).map((l) => l.trim()).find((l) => l && !l.startsWith("#"));
      if (bare && process.env.QODER_PERSONAL_ACCESS_TOKEN === undefined) {
        process.env.QODER_PERSONAL_ACCESS_TOKEN = bare;
      }
    }
    break;
  }
}

const REGIONS = {
  global: "https://openapi.qoder.sh",
  cn: "https://openapi.qoder.com.cn",
};
const REGION_ORDER = ["global", "cn"];
const REQUEST_TIMEOUT_MS = 15000;
const PAT_DOC_URL = "https://qoder.com/account/integrations";
const APP_DATA_DIR = join(homedir(), "AppData", "Roaming", "com.qoder.app.stable");

const ANSI = process.stdout.isTTY
  ? {
      dim: "\x1b[2m",
      bold: "\x1b[1m",
      green: "\x1b[32m",
      yellow: "\x1b[33m",
      red: "\x1b[31m",
      cyan: "\x1b[36m",
      reset: "\x1b[0m",
    }
  : Object.fromEntries(["dim", "bold", "green", "yellow", "red", "cyan", "reset"].map((k) => [k, ""]));

// ---------- desktop credential decryption (Electron safeStorage: DPAPI + AES-256-GCM) ----------

let cachedDpapiKey = null;

function getDpapiKey() {
  if (cachedDpapiKey) return cachedDpapiKey;
  if (process.platform !== "win32") return null;
  const ps = [
    "Add-Type -AssemblyName System.Security",
    "$enc = [Convert]::FromBase64String((Get-Content \"$env:APPDATA\\com.qoder.app.stable\\Local State\" -Raw | ConvertFrom-Json).os_crypt.encrypted_key)",
    "$key = [Security.Cryptography.ProtectedData]::Unprotect($enc[5..($enc.Length-1)], $null, \"CurrentUser\")",
    "[BitConverter]::ToString($key) -replace \"-\",\"\"",
  ].join("; ");
  try {
    const out = execFileSync("powershell.exe", ["-NoProfile", "-Command", ps], {
      encoding: "utf8",
      windowsHide: true,
      stdio: ["ignore", "pipe", "ignore"],
    });
    const hex = out.replace(/[^0-9a-fA-F]/g, "");
    if (hex.length !== 64) return null;
    cachedDpapiKey = Buffer.from(hex, "hex");
    return cachedDpapiKey;
  } catch {
    return null;
  }
}

function decryptSafeStorage(blob, key) {
  const nonce = blob.subarray(3, 15);
  const tag = blob.subarray(blob.length - 16);
  const data = blob.subarray(15, blob.length - 16);
  const d = createDecipheriv("aes-256-gcm", key, nonce);
  d.setAuthTag(tag);
  return Buffer.concat([d.update(data), d.final()]);
}

function loadDesktopAuth() {
  const key = getDpapiKey();
  if (!key) return null;
  try {
    const plain = decryptSafeStorage(readFileSync(join(APP_DATA_DIR, "auth.v1.dat")), key);
    const auth = JSON.parse(plain.toString("utf8"));
    if (auth && typeof auth.token === "string" && auth.token) {
      return { token: auth.token, tokenExpiresAt: auth.expiresAt ?? null };
    }
  } catch {}
  return null;
}

// ---------- response parsing (mirrors the desktop app's own parser) ----------

function isObj(v) {
  return !!v && typeof v === "object" && !Array.isArray(v);
}
function pickStr(obj, keys) {
  for (const k of keys) if (typeof obj?.[k] === "string") return obj[k];
  return null;
}
function pickNum(obj, keys) {
  for (const k of keys) if (typeof obj?.[k] === "number" && Number.isFinite(obj[k])) return obj[k];
  return null;
}
function pickBool(obj, keys) {
  for (const k of keys) if (typeof obj?.[k] === "boolean") return obj[k];
  return null;
}

function normalizePercentage(raw, used, total) {
  if (raw === null) return total > 0 ? (used / total) * 100 : 0;
  if (raw >= 0 && raw <= 1) return raw * 100;
  if (raw >= 0 && raw <= 100) return raw;
  return total > 0 ? (used / total) * 100 : 0;
}

function normalizeQuota(q) {
  if (!isObj(q)) return null;
  const total = pickNum(q, ["total"]) ?? 0;
  const used = pickNum(q, ["used"]) ?? 0;
  const remainingRaw = pickNum(q, ["remaining"]);
  const pct = pickNum(q, ["percentage"]);
  return {
    total,
    used,
    remaining:
      remainingRaw !== null && Number.isFinite(remainingRaw) ? remainingRaw : Math.max(0, total - used),
    percentage: normalizePercentage(pct, used, total),
    unit: pickStr(q, ["unit"]) ?? "credits",
  };
}

function normalizeExpiry(v) {
  if (v === null || !Number.isFinite(v)) return null;
  // epoch seconds vs epoch milliseconds
  const ms = v < 1e12 ? v * 1000 : v;
  return new Date(ms);
}

function parseUsage(body) {
  if (!isObj(body)) throw new Error("响应不是 JSON 对象");
  if (body.displayMode === "enterprise") {
    return {
      displayMode: "enterprise",
      enterpriseUsage: {
        openMode: pickStr(body.enterpriseUsage, ["openMode"]) ?? "",
        detailUrl: pickStr(body.enterpriseUsage, ["detailUrl"]) ?? "",
      },
    };
  }
  if (body.displayMode !== "qoder" || !isObj(body.qoderUsage)) {
    throw new Error(`未知的 displayMode: ${String(body.displayMode)}`);
  }
  const u = body.qoderUsage;
  const orgRaw = u.org_resource_package ?? u.orgResourcePackage ?? u.shared_quota ?? u.sharedQuota;
  const org = normalizeQuota(orgRaw);
  const orgCap = isObj(orgRaw) ? pickNum(orgRaw, ["cap", "total"]) ?? org?.total ?? 0 : 0;
  const packages = Array.isArray(u.dedicated_resource_packages ?? u.dedicatedResourcePackages)
    ? (u.dedicated_resource_packages ?? u.dedicatedResourcePackages).flatMap((p) => {
        if (!isObj(p)) return [];
        const id = pickStr(p, ["id"])?.trim();
        const total = pickNum(p, ["total"]);
        const used = pickNum(p, ["used"]);
        const remaining = pickNum(p, ["remaining"]);
        if (!id || [total, used, remaining].every((v) => v === null)) return [];
        if (total === null || !Number.isFinite(total) || total <= 0) return [];
        const usedN = used !== null && Number.isFinite(used) ? Math.max(0, used) : 0;
        const remN =
          remaining !== null && Number.isFinite(remaining) ? Math.max(0, remaining) : Math.max(0, total - usedN);
        const pct = pickNum(p, ["percentage"]);
        return [
          {
            id,
            name: pickStr(p, ["name"])?.trim() ?? "",
            description: pickStr(p, ["description"])?.trim() ?? "",
            total,
            used: usedN,
            remaining: remN,
            percentage: normalizePercentage(pct, usedN, total),
            unit: pickStr(p, ["unit"]) ?? "credits",
            expiresAt: normalizeExpiry(pickNum(p, ["expires_at", "expiresAt"])),
            available: pickBool(p, ["available"]) ?? false,
            status: pickStr(p, ["status"]) ?? "",
          },
        ];
      })
    : [];

  const totalPctRaw = pickNum(u, ["total_usage_percentage", "totalUsagePercentage"]);
  return {
    displayMode: "qoder",
    userType: pickStr(u, ["user_type", "userType"]) ?? "",
    usageType: pickStr(u, ["usage_type", "usageType"]) ?? "credits",
    isQuotaExceeded: pickBool(u, ["is_quota_exceeded", "isQuotaExceeded"]) ?? false,
    totalUsagePercentage: totalPctRaw === null ? null : normalizePercentage(totalPctRaw, 0, 0),
    expiresAt: normalizeExpiry(pickNum(u, ["expires_at", "expiresAt"])),
    upgradeUrl: pickStr(u, ["upgrade_url", "upgradeUrl"]) ?? "",
    userQuota: normalizeQuota(u.user_quota ?? u.userQuota),
    addOnQuota: normalizeQuota(u.add_on_quota ?? u.addOnQuota),
    orgResourcePackage: org ? { ...org, total: orgCap, cap: orgCap, remaining: Math.max(0, orgCap - org.used), available: pickBool(orgRaw, ["available"]) ?? orgCap > 0 } : null,
    dedicatedResourcePackages: packages,
  };
}

// ---------- rendering ----------

function bar(pct, width = 16) {
  const filled = Math.max(0, Math.min(width, Math.round((pct / 100) * width)));
  return "█".repeat(filled) + "░".repeat(width - filled);
}

function colorForPct(pct) {
  if (pct >= 85) return ANSI.red;
  if (pct >= 60) return ANSI.yellow;
  return ANSI.green;
}

function fmtNum(n) {
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}

function fmtCountdown(date) {
  if (!date) return "";
  const ms = date.getTime() - Date.now();
  if (ms <= 0) return "已过期";
  const days = Math.floor(ms / 86400000);
  const hours = Math.floor((ms % 86400000) / 3600000);
  if (days > 0) return `${days}天${hours}小时后到期`;
  const mins = Math.floor((ms % 3600000) / 60000);
  return `${hours}小时${mins}分后到期`;
}

function fmtDate(date) {
  if (!date) return "-";
  const p = (n) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${p(date.getMonth() + 1)}-${p(date.getDate())} ${p(date.getHours())}:${p(date.getMinutes())}`;
}

function renderQuotaLine(label, q, extra = "") {
  if (!q) return "";
  const pct = Math.max(0, Math.min(100, q.percentage));
  const color = colorForPct(pct);
  return (
    `${label.padEnd(10, " ")} ${color}${bar(pct)} ${pct.toFixed(1).padStart(5)}%${ANSI.reset}  ` +
    `${fmtNum(q.used)}/${fmtNum(q.total)} ${q.unit}  ${ANSI.dim}剩余 ${fmtNum(q.remaining)}${ANSI.reset}${extra}`
  );
}

function render(usage, { updatedAt, source }) {
  const out = [];
  out.push(`${ANSI.bold}Qoder 用量${ANSI.reset}  ${ANSI.dim}${usage.userType || ""}  更新于 ${fmtDate(updatedAt)}${ANSI.reset}`);
  out.push("─".repeat(78));

  if (usage.displayMode === "enterprise") {
    out.push("企业账户：用量详情需在浏览器中查看");
    if (usage.enterpriseUsage.detailUrl) {
      out.push(`${ANSI.cyan}${usage.enterpriseUsage.detailUrl}${ANSI.reset}`);
    }
    return out.join("\n");
  }

  if (usage.isQuotaExceeded) {
    out.push(`${ANSI.red}${ANSI.bold}⚠ 额度已用尽（isQuotaExceeded）${ANSI.reset}`);
  }

  const lines = [];
  if (usage.totalUsagePercentage !== null) {
    const pct = Math.max(0, Math.min(100, usage.totalUsagePercentage));
    lines.push(
      `合计用量   ${colorForPct(pct)}${bar(pct)} ${pct.toFixed(1).padStart(5)}%${ANSI.reset}`
    );
  }
  if (usage.userQuota) lines.push(renderQuotaLine("用户额度", usage.userQuota));
  if (usage.addOnQuota) lines.push(renderQuotaLine("附加额度", usage.addOnQuota));
  if (usage.orgResourcePackage && usage.orgResourcePackage.total > 0) {
    const avail = usage.orgResourcePackage.available ? "" : `  ${ANSI.dim}(不可用)${ANSI.reset}`;
    lines.push(renderQuotaLine("组织资源包", usage.orgResourcePackage, avail));
  }
  for (const p of usage.dedicatedResourcePackages) {
    const name = p.name || p.id;
    const expire = p.expiresAt ? `  ${ANSI.dim}${fmtCountdown(p.expiresAt)}${ANSI.reset}` : "";
    const avail = p.available ? "" : `  ${ANSI.dim}(不可用)${ANSI.reset}`;
    lines.push(renderQuotaLine(name.slice(0, 10), p, `${expire}${avail}`));
  }
  out.push(lines.join("\n") || "（无额度数据）");

  out.push("─".repeat(78));
  if (usage.expiresAt) {
    out.push(`额度周期：${fmtDate(usage.expiresAt)}（${fmtCountdown(usage.expiresAt)}）`);
  }
  if (source) {
    out.push(`${ANSI.dim}凭据来源：${source}${ANSI.reset}`);
  }
  if (usage.upgradeUrl) {
    out.push(`${ANSI.dim}升级：${usage.upgradeUrl}${ANSI.reset}`);
  }
  return out.join("\n");
}

// ---------- API ----------

async function fetchUsage(baseUrl, token) {
  const res = await fetch(`${baseUrl}/sash/api/v2/me/usage`, {
    headers: { Authorization: `Bearer ${token}` },
    redirect: "error",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (res.status === 401 || res.status === 403) {
    const err = new Error(`认证失败 (HTTP ${res.status})`);
    err.authError = true;
    throw err;
  }
  if (!res.ok) throw new Error(`请求失败：HTTP ${res.status}`);
  return parseUsage(await res.json());
}

function authHint(kind, pinnedRegion) {
  const regionNote = pinnedRegion
    ? `当前固定了 --region ${pinnedRegion}，若账户在其他区域请改用 --region global/cn 或省略以自动探测`
    : null;
  const hints = {
    desktop: "桌面应用令牌被拒绝 — 请打开 Qoder 桌面应用确认已登录（应用会自动刷新令牌），或用 --token 显式提供",
    pat: "PAT 认证失败 — 用量 API 目前不接受 Personal Access Token（实测 TOKEN_EXPIRE），请改用桌面应用凭据",
    arg: "认证失败 — 请检查 --token 提供的令牌",
  };
  const hint = hints[kind] ?? hints.arg;
  return regionNote && kind !== "pat" ? `${hint}；${regionNote}` : hint;
}

function buildCandidates(args) {
  const candidates = [];
  if (args.token) {
    candidates.push({ kind: "arg", token: args.token, label: "--token" });
  } else {
    const desktop = loadDesktopAuth();
    if (desktop) {
      const exp = desktop.tokenExpiresAt ? `，令牌有效期至 ${desktop.tokenExpiresAt.slice(0, 10)}` : "";
      candidates.push({ kind: "desktop", token: desktop.token, label: `桌面应用令牌（auth.v1.dat${exp}）` });
    }
    const pat = process.env.QODER_PERSONAL_ACCESS_TOKEN;
    if (pat) candidates.push({ kind: "pat", token: pat, label: "PAT（QODER_PERSONAL_ACCESS_TOKEN）" });
  }
  return candidates;
}

async function resolveAndFetch(args, cachedRegion) {
  const regionOrder = args.region
    ? [args.region]
    : cachedRegion
      ? [cachedRegion, ...REGION_ORDER.filter((r) => r !== cachedRegion)]
      : REGION_ORDER;
  const errors = [];
  const candidates = buildCandidates(args);
  for (const cand of candidates) {
    for (const region of regionOrder) {
      try {
        const usage = await fetchUsage(REGIONS[region], cand.token);
        return { usage, cand, region };
      } catch (err) {
        const msg = err?.name === "TimeoutError" ? `请求超时（${REQUEST_TIMEOUT_MS / 1000}s）` : err?.message ?? String(err);
        errors.push({ cand, region, msg, authError: !!err?.authError });
      }
    }
  }
  if (candidates.length === 0) {
    const e = new Error(
      `缺少凭据：未找到桌面应用凭据（%APPDATA%\\com.qoder.app.stable\\auth.v1.dat），也未设置 QODER_PERSONAL_ACCESS_TOKEN 或 --token`
    );
    e.noCredentials = true;
    throw e;
  }
  const detail = errors
    .map((e) => `  [${e.cand.kind} → ${e.region}] ${e.msg}${e.authError ? `（${authHint(e.cand.kind, args.region)}）` : ""}`)
    .join("\n");
  const e = new Error(`所有凭据来源均失败：\n${detail}`);
  e.noCredentials = candidates.length === 0;
  throw e;
}

// ---------- demo ----------

const DEMO_BODY = {
  displayMode: "qoder",
  qoderUsage: {
    user_type: "pro",
    usage_type: "credits",
    is_quota_exceeded: false,
    total_usage_percentage: 0.42,
    expires_at: Date.now() + 26 * 86400000 + 5 * 3600000,
    upgrade_url: "https://qoder.com/pricing",
    user_quota: { total: 1000, used: 523.5, remaining: 476.5, percentage: 0.5235, unit: "credits" },
    add_on_quota: { total: 500, used: 75, remaining: 425, unit: "credits" },
    dedicated_resource_packages: [
      {
        id: "pkg-booster-01",
        name: "加速包",
        description: "限时加速额度包",
        total: 200,
        used: 30,
        remaining: 170,
        percentage: 0.15,
        unit: "credits",
        expires_at: Date.now() + 29 * 86400000,
        available: true,
        status: "QUOTA_DETAIL_STATUS_ACTIVE",
      },
      {
        id: "pkg-gift-02",
        name: "赠送包",
        total: 100,
        used: 92,
        remaining: 8,
        percentage: 0.92,
        unit: "credits",
        expires_at: Date.now() + 3 * 86400000,
        available: true,
        status: "QUOTA_DETAIL_STATUS_ACTIVE",
      },
    ],
  },
};

// ---------- CLI ----------

function parseArgs(argv) {
  const args = { watch: 0, _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case "--demo": args.demo = true; break;
      case "--json": args.json = true; break;
      case "--watch": {
        const next = argv[i + 1];
        if (next && !next.startsWith("--") && Number.isFinite(Number(next))) {
          args.watch = Math.max(5, Number(next));
          i++;
        } else {
          args.watch = 300;
        }
        break;
      }
      case "--token": args.token = argv[++i]; break;
      case "--region": args.region = argv[++i]; break;
      case "--help": case "-h": args.help = true; break;
      default: args._.push(a);
    }
  }
  return args;
}

const HELP = `用法: node qoder-usage.mjs [选项]

选项:
  --token <token>    显式指定令牌（优先级最高）
  --region <name>    global（openapi.qoder.sh）或 cn（openapi.qoder.com.cn）；默认自动探测
  --watch [sec]      持续轮询，默认间隔 300 秒（最小 5 秒）
  --json             输出原始 JSON
  --demo             使用内置示例数据（无需凭据）
  --help             显示帮助

凭据来源（按顺序自动尝试，命中即用）:
  1. 桌面应用令牌    自动解密 %APPDATA%\\com.qoder.app.stable\\auth.v1.dat（推荐，需已登录 Qoder 桌面应用）
  2. PAT             环境变量 QODER_PERSONAL_ACCESS_TOKEN 或 .env 文件（实测对用量 API 无效，仅作保留）

说明: PAT 创建地址 ${PAT_DOC_URL}，但用量端点只接受桌面应用的 OAuth 令牌。`;

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const hasCredentials = !!(args.token ?? process.env.QODER_PERSONAL_ACCESS_TOKEN) || process.platform === "win32";
  if (args.help || (process.argv.length <= 2 && !hasCredentials)) {
    console.log(HELP);
    return;
  }

  if (args.demo) {
    if (args.json) {
      console.log(JSON.stringify(DEMO_BODY, null, 2));
      return;
    }
    console.log(render(parseUsage(DEMO_BODY), { updatedAt: new Date(), source: "演示数据" }));
    return;
  }

  let cachedRegion = null;
  const poll = async () => {
    try {
      const { usage, cand, region } = await resolveAndFetch(args, cachedRegion);
      cachedRegion = region;
      if (args.json) {
        console.log(JSON.stringify(usage, null, 2));
      } else {
        console.clear?.();
        console.log(render(usage, { updatedAt: new Date(), source: `${cand.label} → ${region} (${REGIONS[region]})` }));
      }
    } catch (err) {
      const msg = err?.name === "TimeoutError" ? `请求超时（${REQUEST_TIMEOUT_MS / 1000}s）` : err?.message ?? String(err);
      console.error(`${ANSI.red}错误: ${msg}${ANSI.reset}`);
      if (!args.watch) process.exitCode = 1;
    }
  };

  await poll();
  if (args.watch > 0) {
    console.log(`\n${ANSI.dim}每 ${args.watch} 秒刷新一次，Ctrl+C 退出${ANSI.reset}`);
    setInterval(poll, args.watch * 1000);
  }
}

main();
