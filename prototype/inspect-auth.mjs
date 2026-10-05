// 脱敏检查 ~/.qoder/.auth 的结构：只打印文件名、JSON 键名、类型和字符串长度。
// 不打印任何凭据值（字符串只显示前 2 个字符 + 长度；数字会显示——用于判断过期时间戳）。
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const dir = join(homedir(), ".qoder", ".auth");

function redact(value, key = "") {
  if (typeof value === "string") {
    const prefix = value.slice(0, 2).replace(/[^A-Za-z0-9_-]/g, "?");
    return `<str len=${value.length} prefix=${prefix}>`;
  }
  if (typeof value === "number" || typeof value === "boolean") return value;
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    return [redact(value[0], key + "[0]"), `...(${value.length} items)`];
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, redact(v, k)]));
  }
  return value === null ? null : `<${typeof value}>`;
}

for (const name of readdirSync(dir)) {
  const full = join(dir, name);
  const st = statSync(full);
  if (st.isDirectory()) {
    console.log(`${name}/ (directory, ${readdirSync(full).length} entries)`);
    continue;
  }
  console.log(`\n=== ${name} (${st.size} bytes) ===`);
  const raw = readFileSync(full, "utf8").trim();
  if (!raw) { console.log("(empty)"); continue; }
  const head = raw.slice(0, 1);
  if (head === "{" || head === "[") {
    try {
      console.log(JSON.stringify(redact(JSON.parse(raw)), null, 2));
      continue;
    } catch { /* not JSON, fall through */ }
  }
  // 非 JSON：只显示首字符类型和长度
  const isPrintable = /^[\x20-\x7e]+$/.test(raw.slice(0, 64));
  console.log(`(non-JSON, first char: ${JSON.stringify(head)}, printable: ${isPrintable}, length: ${raw.length})`);
}
