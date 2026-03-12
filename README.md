# font_obfuscator

[中文](#中文) | [English](#english)

---

## 中文

### 简介

字体混淆反爬虫工具。通过重映射字体中的字形（glyph），使 HTML 中的文本与用户看到的内容不同，从而防止爬虫直接抓取页面文字。

支持混淆英文、数字及大部分 CJK（中日韩）字符，生成 TTF 和 WOFF2 格式的字体文件。

使用 Rust 实现，基于 Google [fontations](https://github.com/googlefonts/fontations) 项目进行字体解析和构建。

### 原理

**普通混淆**：用户提供明文和阴书（长度相同），生成一个自定义字体，其中阴书字符的 Unicode 码位指向明文字符的字形。HTML 中写阴书文本，浏览器加载自定义字体后渲染出明文内容，爬虫只能读到阴书。

**加强混淆**：只需提供明文，自动使用 Unicode Private Use Area（U+E000-U+F8FF）的随机码位进行映射，并返回对应的 HTML entity 编码。爬虫无法通过 Unicode 表反查字符含义。

### 快速开始

```bash
# 构建
cargo build --release

# 运行（默认监听 127.0.0.1:1323）
./target/release/font_obfuscator

# 自定义端口
PORT=8080 ./target/release/font_obfuscator
```

### 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | `1323` | 服务监听端口 |
| `LISTEN_ADDR` | `127.0.0.1` | 服务监听地址 |
| `BASE_FONT_PATH` | `base-font/KaiGenGothicCN-Regular.ttf` | 基础字体文件路径 |

### API

#### GET /

健康检查，返回 `it works`。

#### POST /api/encrypt

普通混淆（明文 + 阴书）。生成的字体只包含映射的字符。

设置 `keep_all: true` 可保留原字体中的所有字符，仅替换指定字符的字形（[#97](https://github.com/solarhell/font_obfuscator/issues/97)）。

```bash
curl -X POST http://127.0.0.1:1323/api/encrypt \
  -H 'Content-Type: application/json' \
  -d '{
    "plaintext": "真0123456789好",
    "shadowtext": "假6982075431的",
    "only_ttf": false,
    "keep_all": false
  }'
```

响应：

```json
{
  "message": "success",
  "hint": "",
  "response": {
    "base64ed": {
      "ttf": "AAEAAAALAIAAAwA...",
      "woff2": "d09GMgABAAAAA..."
    }
  }
}
```

#### POST /api/encrypt-plus

加强混淆（仅需明文）。

```bash
curl -X POST http://127.0.0.1:1323/api/encrypt-plus \
  -H 'Content-Type: application/json' \
  -d '{
    "plaintext": "价格998元",
    "only_ttf": false
  }'
```

响应：

```json
{
  "message": "success",
  "hint": "",
  "response": {
    "base64ed": {
      "ttf": "AAEAAAALAIAAAwA...",
      "woff2": "d09GMgABAAAAA..."
    },
    "html_entities": {
      "价": "&#xeafb",
      "格": "&#xee75",
      "9": "&#xe104",
      "8": "&#xf349",
      "元": "&#xf0d0"
    }
  }
}
```

### 前端使用示例

```html
<style>
  @font-face {
    font-family: 'ObfuscatedFont';
    src: url(data:font/woff2;base64,d09GMgABAAAAA...) format('woff2');
  }
  .protected {
    font-family: 'ObfuscatedFont';
  }
</style>

<!-- 普通混淆：HTML 中写阴书，用户看到明文 -->
<span class="protected">假6982075431的</span>
<!-- 用户看到的是：真0123456789好 -->

<!-- 加强混淆：HTML 中写 entity 编码 -->
<span class="protected">&#xeafb;&#xee75;&#xe104;&#xe104;&#xf349;&#xf0d0;</span>
<!-- 用户看到的是：价格998元 -->
```

---

## English

### Introduction

A font obfuscation tool for anti-scraping. It remaps glyphs in a font so that the text in HTML differs from what users actually see, preventing crawlers from directly extracting page content.

Supports obfuscation of English, digits, and most CJK characters. Generates TTF and WOFF2 font files.

Built with Rust, using Google's [fontations](https://github.com/googlefonts/fontations) project for font parsing and building.

### How It Works

**Basic obfuscation**: Provide plaintext and shadow text (same length). A custom font is generated where shadow characters' Unicode codepoints map to plaintext characters' glyphs. The HTML contains shadow text, but the browser renders plaintext when the custom font is loaded. Crawlers can only read the shadow text.

**Enhanced obfuscation**: Only plaintext is needed. Random codepoints from Unicode Private Use Area (U+E000-U+F8FF) are used for mapping, and corresponding HTML entity codes are returned. Crawlers cannot reverse-lookup character meanings from Unicode tables.

### Quick Start

```bash
# Build
cargo build --release

# Run (listens on 127.0.0.1:1323 by default)
./target/release/font_obfuscator

# Custom port
PORT=8080 ./target/release/font_obfuscator
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `1323` | Server listening port |
| `LISTEN_ADDR` | `127.0.0.1` | Server listen address |
| `BASE_FONT_PATH` | `base-font/KaiGenGothicCN-Regular.ttf` | Base font file path |

### API

#### GET /

Health check, returns `it works`.

#### POST /api/encrypt

Basic obfuscation (plaintext + shadow text). The generated font only contains the mapped characters.

Set `keep_all: true` to preserve all characters from the original font, only replacing the specified character glyphs ([#97](https://github.com/solarhell/font_obfuscator/issues/97)).

```bash
curl -X POST http://127.0.0.1:1323/api/encrypt \
  -H 'Content-Type: application/json' \
  -d '{
    "plaintext": "real0123456789content",
    "shadowtext": "fake6982075431garbage",
    "only_ttf": false,
    "keep_all": false
  }'
```

Response:

```json
{
  "message": "success",
  "hint": "",
  "response": {
    "base64ed": {
      "ttf": "AAEAAAALAIAAAwA...",
      "woff2": "d09GMgABAAAAA..."
    }
  }
}
```

#### POST /api/encrypt-plus

Enhanced obfuscation (plaintext only).

```bash
curl -X POST http://127.0.0.1:1323/api/encrypt-plus \
  -H 'Content-Type: application/json' \
  -d '{
    "plaintext": "price998usd",
    "only_ttf": false
  }'
```

Response:

```json
{
  "message": "success",
  "hint": "",
  "response": {
    "base64ed": {
      "ttf": "AAEAAAALAIAAAwA...",
      "woff2": "d09GMgABAAAAA..."
    },
    "html_entities": {
      "p": "&#xeafb",
      "r": "&#xee75",
      "i": "&#xe104",
      "c": "&#xf349",
      "e": "&#xf0d0",
      "9": "&#xe832",
      "8": "&#xef11",
      "u": "&#xe5a3",
      "s": "&#xf721",
      "d": "&#xe009"
    }
  }
}
```

### Frontend Usage

```html
<style>
  @font-face {
    font-family: 'ObfuscatedFont';
    src: url(data:font/woff2;base64,d09GMgABAAAAA...) format('woff2');
  }
  .protected {
    font-family: 'ObfuscatedFont';
  }
</style>

<!-- Basic: HTML contains shadow text, user sees plaintext -->
<span class="protected">fake6982075431garbage</span>
<!-- User sees: real0123456789content -->

<!-- Enhanced: HTML contains entity codes -->
<span class="protected">&#xeafb;&#xee75;&#xe104;&#xf349;&#xf0d0;&#xe832;&#xe832;&#xef11;&#xe5a3;&#xf721;&#xe009;</span>
<!-- User sees: price998usd -->
```

## License

MIT
