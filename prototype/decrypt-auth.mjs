// 解密 Qoder 的 auth.v1.dat 并只打印脱敏结构（不含任何凭据值）
// 密钥通过环境变量 DPAPI_KEY_HEX 传入，不打印。
import { readFileSync } from "node:fs";
import { createDecipheriv } from "node:crypto";
import { join } from "node:path";
import { homedir } from "node:os";

const dir = join(homedir(), "AppData", "Roaming", "com.qoder.app.stable");

function decrypt(blob, key) {
  // v10(3) + nonce(12) + ciphertext + tag(16)
  const nonce = blob.subarray(3, 15);
  const tag = blob.subarray(blob.length - 16);
  const data = blob.subarray(15, blob.length - 16);
  const d = createDecipheriv("aes-256-gcm", key, nonce);
  d.setAuthTag(tag);
  return Buffer.concat([d.update(data), d.final()]);
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

const key = Buffer.from(process.env.DPAPI_KEY_HEX, "hex");
if (key.length !== 32) throw new Error(`key length ${key.length} != 32`);

for (const name of ["auth.v1.dat", "auth-profile-overlays.v1.dat"]) {
  try {
    const plain = decrypt(readFileSync(join(dir, name)), key);
    const text = plain.toString("utf8");
    const json = JSON.parse(text);
    console.log(`\n=== ${name} 解密成功 ===`);
    console.log(JSON.stringify(redact(json), null, 2));
  } catch (err) {
    console.log(`\n=== ${name}: ${err.message}`);
  }
}
