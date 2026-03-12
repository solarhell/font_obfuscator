# fontObfuscator Rust 重写规格

## 项目目标

将 Python 实现的字体混淆反爬虫工具完整迁移到 Rust，保持 API 兼容性（去除 OSS 上传功能）。

## 目录结构

```
font-obfuscator-rs/
├── Cargo.toml
├── src/
│   ├── main.rs          # 入口 + axum Web 服务
│   ├── config.rs        # 配置（环境变量 / 默认值）
│   ├── core.rs          # 核心混淆逻辑：obfuscate + obfuscate_plus
│   ├── utils.rs         # 工具函数（校验、去重、base64）
│   └── model.rs         # API 请求/响应结构体
├── base-font/           # -> ../base-font 符号链接
│   └── KaiGenGothicCN-Regular.ttf
└── output/              # 临时文件输出目录（运行时创建）
```

## 任务清单

### Phase 1: 项目脚手架
- [x] 阅读并理解 Python 源码
- [x] 1.1 创建 `font-obfuscator-rs/` 目录和 `Cargo.toml`
- [x] 1.2 搭建模块结构（main/config/core/utils/model）

### Phase 2: 配置与模型
- [x] 2.1 `config.rs` — 配置结构体，支持环境变量
- [x] 2.2 `model.rs` — 请求/响应 serde 结构体

### Phase 3: 工具函数
- [x] 3.1 `utils.rs` — str_has_whitespace / str_has_emoji / deduplicate_str / base64_binary

### Phase 4: 核心字体混淆
- [x] 4.1 `core.rs` — `obfuscate()` 函数（read-fonts 解析 + write-fonts 构建）
- [x] 4.2 `core.rs` — `obfuscate_plus()` 函数（Private Use Area 映射）
- [x] 4.3 TTF → WOFF2 转换（ttf2woff2 crate）

### Phase 5: Web 服务
- [x] 5.1 `main.rs` — axum Web 服务（GET / + POST /api/encrypt + POST /api/encrypt-plus）

### Phase 6: 验证
- [x] 6.1 编译通过（edition 2024）
- [x] 6.2 API 端到端测试通过
- [x] 6.3 生成的字体文件可被 fonttools 正确解析
- [x] 6.4 错误处理验证（空格/emoji/长度不一致/相同文本）

### 待办
- [ ] Dockerfile（多阶段构建）
- [ ] 集成测试（cargo test）

## 实际技术栈

```toml
[dependencies]
axum = "0.8.8"
tokio = { version = "1.50.0", features = ["full"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.149"
read-fonts = "0.37.0"       # Google fontations - 读取 TTF
write-fonts = "0.45.0"      # Google fontations - 构建 TTF
ttf2woff2 = "0.11.0"        # TTF → WOFF2 转换
base64 = "0.22.1"
uuid = { version = "1.22.0", features = ["v4"] }
rand = "0.10.0"
thiserror = "2.0.18"
tower-http = { version = "0.6.8", features = ["cors"] }
tracing = "0.1.44"
tracing-subscriber = "0.3.22"
```

## API 接口

### POST /api/encrypt
```json
// Request
{ "plaintext": "真实内容", "shadowtext": "混淆内容", "only_ttf": false }
// Response
{ "message": "success", "hint": "", "response": { "base64ed": { "ttf": "...", "woff2": "..." } } }
```

### POST /api/encrypt-plus
```json
// Request
{ "plaintext": "真实内容", "only_ttf": false }
// Response
{ "message": "success", "hint": "", "response": { "base64ed": { "ttf": "...", "woff2": "..." }, "html_entities": { "真": "&#xe1a2", "实": "&#xf033" } } }
```

## 核心算法说明

**obfuscate 的本质**：在新字体中，shadow 字符的 Unicode 码位指向 plain 字符的字形。
浏览器加载此字体后，HTML 中写 shadow 文本 → 渲染出 plain 的字形 → 用户看到的是 plain 内容。
爬虫直接读 HTML 只能拿到 shadow 文本。

**obfuscate_plus 的增强**：不使用真实 Unicode 码位，而是用 Private Use Area 的随机码位，
使得 HTML 中的字符对爬虫来说完全无意义（无法通过 Unicode 表反查）。
