// 脱敏检查 Electron 数据目录中的 auth 文件结构（不打印凭据值）
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const dir = join(homedir(), "AppData", "Roaming", "com.qoder.app.stable");

function describe(full) {
  const buf = readFileSync(full);
  console.log(`\n=== ${full.split("\\").pop()} (${buf.length} bytes) ===`);
  // 检查是否 DPAPI/safeStorage 加密（通常以 v10/v11 开头）或明文 JSON
  const head = buf.subarray(0, 3).toString("latin1");
  if (head === "v10" || head === "v11") {
    console.log(`加密格式: Electron safeStorage (DPAPI)，前缀 "${head}"，无法直接读取`);
    return;
  }
  const text = buf.toString("utf8").trim();
  const first = text[0];
  if (first === "{" || first === "[") {
    try {
      const json = JSON.parse(text);
      console.log("明文 JSON，脱敏结构:");
      console.log(JSON.stringify(redact(json), null, 2));
      return;
    } catch (e) {
      console.log("看起来是 JSON 但解析失败:", e.message);
      return;
    }
  }
  console.log(`未知格式：前 3 字节 hex = ${buf.subarray(0, 3).toString("hex")}`);
}

function redact(value) {
  if (typeof value === "string") {
    const prefix = value.slice(0, 2).replace(/[^A-Za-z0-9_-]/g, "?");
    return `<str len=${value.length} prefix=${prefix}>`;
  }
  if (typeof value === "number" || typeof value === "boolean") return value;
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    return [redact(value[0]), `...(${value.length} items)`];
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, redact(v)]));
  }
  return value;
}

for (const name of ["auth.v1.dat", "auth-profile-overlays.v1.dat", "auth.machine-id"]) {
  try {
    describe(join(dir, name));
  } catch (err) {
    console.log(`${name}: ${err.message}`);
  }
}
