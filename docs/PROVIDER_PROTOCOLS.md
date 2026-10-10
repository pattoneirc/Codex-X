# 供应商协议与 Headers

## 选择上游协议

在「供应商 → 新增 / 编辑」选择上游实际提供的协议：

| 上游接口协议 | 上游请求 | 默认认证 | 需要本地路由 |
| --- | --- | --- | --- |
| OpenAI Responses | `/v1/responses` | Bearer API Key | 否 |
| OpenAI Chat Completions | `/v1/chat/completions` | Bearer API Key | 是 |
| Claude Messages | `/v1/messages` | `x-api-key`、`anthropic-version` | 是 |
| Gemini generateContent | `/v1beta/models/{model}:generateContent` 或 `:streamGenerateContent` | `x-goog-api-key` | 是 |

Base URL 可以包含服务的路径前缀。Gemini 地址已有 `/v1` 或 `/v1beta` 时使用该版本，不重复添加。
模型名称必须是该供应商支持的名称；获取模型列表和连接检测也使用所选协议及 Headers。

Chat、Claude、Gemini 供应商可以先保存，再到「设置 → 路由与故障转移」开启路由总开关和 Codex 请求接管，随后启用供应商。
Codex 始终使用 Responses，本地路由负责转换请求及普通 / 流式响应。自动切换是独立选项，不是使用协议转换的前提。

协议转换期间保持 Codex-X 运行。关闭路由或取消接管前，请先切回 Responses 供应商或官方账号。退出期间 Codex 请求会暂停，重开 Codex-X 后自动恢复本地转换路由。在线更新会保留转换协议和恢复记录，安装后重开恢复；无需为了更新强制切换供应商。
应用不会将 Claude / Gemini 原生地址恢复成不能使用的 Responses 直连地址；异常退出后会按保存的配置重新启动路由。
端口被占用或配置损坏时会明确提示，需要修复路由或切换供应商。

上游协议保存在 Codex-X 的供应商记录中；生成的 Codex TOML 使用 `wire_api = "responses"`。
旧的 Chat 配置会识别为 Chat Completions 转换，复制供应商和再次导入保留协议选择。

## 转换范围

- 转换文字、图片、函数工具、命名空间工具及 custom 工具（包括 `apply_patch`）的输入输出。
- 支持 JSON 与 SSE 响应、工具参数增量、用量及结束状态；已向 Codex 输出内容后不会重放到其他供应商。
- Claude 的思考预算受 `max_output_tokens` 和上游模型能力约束。Gemini 3 使用相应模型支持的 `thinkingLevel`；其他 Gemini 模型的非 `none` 强度使用上游动态思考预算，强度不是与 OpenAI 一一对应的保证。
- Gemini 思考签名跟随对应模型消息回放；不能把其他协议的加密推理状态当成 Gemini 签名。
- 不支持的参数、媒体形式或上游返回内容会报告具体兼容问题，不会静默丢弃后显示成功。Gemini 不接受无法原生保证的 `strict = true` 工具声明；普通 Codex 内置 shell 工具使用 `strict = false`。
- Claude / Gemini 原生图片输入支持 `detail = auto`，不接受无法对应的 `high` / `low` 图像精度参数。本地 base64 图片直接转换；远程文件 URI 由上游检验，路由不会擅自下载文件。
- Claude 未指定输出上限时使用 8192 tokens。Gemini 禁止并行工具的请求会在本地检查；上游违规返回多个调用时明确报错，不会执行多个或丢弃其中一个。

转换协议不提供 Responses 的服务端会话状态、后台任务、托管工具或 `/compact` 接口。
带 `previous_response_id`、不兼容的加密状态等请求需要完整对话、新建会话或使用原生 Responses 供应商。
上游仍可能拒绝不支持的模型、工具或采样组合；选择协议不会增加模型本身的能力。

## 编辑 Headers

在供应商表单中添加固定值或环境变量来源的 Header。

`User-Agent` 用于客户端标识，可手动填写或明确选择 Claude Code / Kilo Code 预设。预设只添加或更新一个 `User-Agent` 固定值，其他请求头保留；不选预设就保持现有设置。预设值来自本次核对的 CC Switch 配置，不保证每家供应商均接受，也不会填入任何 API Key。

填写示例随名称变化：`HTTP-Referer` 填应用网址，`X-Title` / `X-OpenRouter-Title` 填应用名称，认证头按供应商文档填写。`X-Title: Codex-X` 表示应用名称，是合法的可选标识；它不承担 User-Agent 客户端校验的作用。占位提示不会作为实际值保存或发送。

CC Switch 的 Codex 页面将 UA 保存在 `meta.customUserAgent`，日常转发通过本地路由生效；其通用 Headers 键值编辑器主要用于 OpenCode / Pi。Codex-X 将这些字段保存到 Codex 原生 `http_headers` / `env_http_headers`：直连由 Codex 发送，路由模式由 Codex-X 发送。
同一个名称不能重复（忽略大小写），字段错误直接显示在对应行。固定值保留原有空格。
环境变量模式仅保存变量名：直连时由 Codex 读取，经过路由时由 Codex-X 读取，请在启动应用前设置变量。

直连使用 `Authorization` Header 认证时请留空 API Key；显式 API Key 会替换旧的 Authorization。
本地路由使用当前上游供应商自己的认证和 Headers，供应商的显式认证 Header 优先于默认认证。
本地路由不允许自定义 Host、Cookie、连接控制头或内部路由认证头，不会把 Codex 的本地路由令牌发送给上游。

Headers 保存在已有的 TOML `http_headers` / `env_http_headers` 字段；编辑会保留其他 TOML 字段及注释。
