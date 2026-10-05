# Qoder Usage Monitor

监控 Qoder 账户用量（credits 额度、附加包、到期倒计时）的工具。灵感来自 [codex-usage-monitor](https://github.com/upstream-ray/codex-usage-monitor)，但 Qoder 是 credits 制（带到期时间），无 Codex 式 5 小时滚动窗口，UI 以「剩余 credits + 到期倒计时」为核心。

## 当前状态

- [x] Rust 任务栏小组件（`src/`，`qoder-usage.exe`，2026-10-05 真机验证：内嵌任务栏 + 真实凭据轮询）
- [x] CLI 原型（`prototype/qoder-usage.mjs`，零依赖 Node.js 18+）
- [x] 真实数据联调（桌面应用凭据自动解密 + region 自动探测，2026-10-05 验证通过）

## 任务栏小组件（Rust）

纯 Win32 分层窗口（`windows` crate，无 WebView / 无运行时依赖，release 单文件约 700 KB），
移植 codex-usage-monitor 的任务栏内嵌骨架，按 Qoder 的 credits 模型重排了信息层级。

```powershell
cd D:\Projects\qoder-usage-monitor
cargo build --release
start target\release\qoder-usage.exe        # 已登录 Qoder 桌面应用即可，零配置
```

### 命令行参数

| 参数 | 说明 |
|---|---|
| `--demo` | 内置演示数据，无需凭据，便于查看界面 |
| `--preview` | 以独立顶层窗口显示，方便截图与调试 |
| `--diagnose` | 运行日志写入 `%TEMP%\qoder-usage.log` |
| `--token <token>` | 显式提供桌面应用 OAuth 令牌（等价于环境变量 `QODER_USAGE_TOKEN`） |
| `--region <name>` | `global` / `cn`，默认自动探测并缓存 |
| `--help` | 显示帮助 |

进程以 `Global\QoderUsage` 互斥量保证单实例；`explorer.exe` 重启后会自动重新内嵌。

### 界面

两行布局、共享标签列，随内容自动收缩宽度：

- **额度**：用户额度 / 附加额度 / 组织额度 / 专属资源包，最多三条并排，每条 10 段进度条 + 数值文本（剩余或用尽）
- **到期**：周期时间进度条 + 倒计时（如 `26天4时`），到期时间同时出现在托盘提示里

可切换：分段 / 连续进度条、跟随系统 / 浅色 / 深色主题、透明背景（贴合任务栏）、中 / 英文、
每行是否显示、轮询频率（1 / 5 / 15 / 60 分钟）、低额度预警阈值（关 / 10% / 25% / 50%，触发时托盘气泡提示）。

### 交互

- 默认停靠在**通知区左侧**（任务栏上唯一不会被系统按钮遮挡的位置）；按住左键拖动可改位置，位置写入设置文件
- 右键小组件或托盘图标 → 菜单；左键托盘图标 → 显示 / 隐藏小组件；菜单「重置位置」恢复自动停靠
- 菜单「打开用量详情」跳转 `enterpriseUsage.detailUrl` 对应的 Qoder 网页

### 配置与日志

- 设置：`%APPDATA%\QoderUsage\settings.json`（首次修改任选项时生成；`anchor_offset` 缺省即自动停靠）
- 开机自启：HKCU `\Software\Microsoft\Windows\CurrentVersion\Run` 的 `QoderUsage` 值
- 诊断日志：`%TEMP%\qoder-usage.log`（仅 `--diagnose` 时写入）

### 已知限制

- 未做自动更新 / 安装包，需手动替换 exe
- `displayMode: "enterprise"` 的账户没有配额明细，小组件只显示查看链接提示
- 组织额度与专属资源包取决于账户是否开通

## CLI 原型用法

```powershell
cd D:\Projects\qoder-usage-monitor\prototype

# 内置示例数据，无需凭据
node qoder-usage.mjs --demo

# 真实数据（已登录 Qoder 桌面应用即可，零配置）
node qoder-usage.mjs

# 持续轮询（每 5 分钟）
node qoder-usage.mjs --watch 300

# 输出解析后的 JSON / 固定区域
node qoder-usage.mjs --json
node qoder-usage.mjs --region global   # 或 cn；默认自动探测
```

实测输出（Pro Trial 账户）：

```text
Qoder 用量  personal_professional_trial  更新于 2026-10-05 09:44
──────────────────────────────────────────────────────────────────────────────
合计用量   ████████░░░░░░░░  51.0%
用户额度       ██████████████░░  85.0%  253/300 credits  剩余 47
附加额度       ░░░░░░░░░░░░░░░░   0.0%  0/200 credits  剩余 200
──────────────────────────────────────────────────────────────────────────────
额度周期：2026-10-14 17:52（9天8小时后到期）
凭据来源：桌面应用令牌（auth.v1.dat，令牌有效期至 2026-11-04） → global (https://openapi.qoder.sh)
```

## 逆向出的 API（截至 2026-10-05，无官方文档，可能随版本变动）

| 端点 | 说明 |
|---|---|
| `GET {base}/sash/api/v2/me/usage` | 用量主接口，`Authorization: Bearer <OAuth token>` |
| `GET {base}/api/v2/user/plan` | 套餐信息（userType、trial 起止、feature flags） |

- base：global 为 `https://openapi.qoder.sh`，cn 为 `https://openapi.qoder.com.cn`；令牌只在签发区域有效（实测 global 令牌对 cn 端点返回 401），CLI 默认自动探测
- 响应 `displayMode: "qoder"` 时含 `qoderUsage.{usageType, isQuotaExceeded, totalUsagePercentage, userQuota, addOnQuota, orgResourcePackage, dedicatedResourcePackages[]}`，每项 `{total, used, remaining, percentage(0-1), unit:"credits", expiresAt}`；`"enterprise"` 时只返回外部查看链接
- 字段兼容 snake_case / camelCase 两种风格

## 认证（重要结论）

**PAT 对用量 API 无效。** 实测 `pt-` 前缀的 Personal Access Token 对所有 openapi/sash 端点均返回 `401 TOKEN_EXPIRE "token is not active"`；官方文档也只把 PAT 用于 CLI 推理接口。用量端点只接受桌面应用的 OAuth access token。

因此 CLI 与小组件采用与 codex-usage-monitor 相同的模式——读本地凭据：

1. **桌面应用凭据（默认，推荐）**：解密 `%APPDATA%\com.qoder.app.stable\auth.v1.dat`
   - Electron safeStorage 格式：`v10` 头（3B）+ nonce（12B）+ AES-256-GCM 密文 + tag（16B）
   - 密钥存于同目录 `Local State` 的 `os_crypt.encrypted_key`（base64 → 剥离 `DPAPI` 前缀 5B → DPAPI 解密，仅当前用户可解）
   - 明文为 JSON：`{schemaVersion, token(dt-), refreshToken(dr-), expiresAt, refreshTokenExpiresAt, user}`
   - access token 有效期约 30 天，refresh token 约 1 年；令牌过期时打开 Qoder 桌面应用会自动刷新并重写该文件，CLI 每次轮询重新解密即可拿到新令牌
2. `--token` / `QODER_USAGE_TOKEN` 显式指定令牌（优先于本地凭据）
3. `QODER_PERSONAL_ACCESS_TOKEN` 环境变量 / `.env`（仅 CLI 原型保留兼容，实际会被 401 拒绝）

## 隐私

- 凭据只在本机内存中使用，不打印、不落盘、不上传；小组件的诊断日志只记录来源标签（文件名 + 令牌有效期），不含令牌本身
- 只调用上述两个只读端点；不写任何凭据文件（令牌刷新由 Qoder 桌面应用自身完成）
- 解密仅使用 Windows 当前用户的 DPAPI 作用域，其他账户无法解密
