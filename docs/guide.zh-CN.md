# Cursor Agent Service 使用教程

用自己的 API Key 和模型，在 Cursor 的 Agent 对话里调用第三方模型。模型配置放在本地 YAML 文件里，服务以命令行程序运行。

## 1. 构建一次

先安装 [Rust stable](https://rustup.rs/) 和本机编译工具。macOS 可用 `xcode-select --install` 安装命令行工具。Debian 或 Ubuntu 安装 `build-essential`、`pkg-config`、`libssl-dev` 和 `cmake`。Windows 使用 Rust MSVC 工具链和 Visual Studio C++ Build Tools。

~~~sh
git clone https://github.com/martinh4cker/cursor-agent-service.git
cd cursor-agent-service
cargo build --release --locked -p cursor-server
cp config.example.yaml config.yaml
~~~

以后直接运行 `./target/release/cursor-server`。Windows 下用 `.\target\release\cursor-server.exe`，复制文件用 `Copy-Item config.example.yaml config.yaml`。

## 2. 配置模型

打开 `config.yaml`，修改 `models` 下的条目。可以复制一个条目来添加第二个模型。每个模型可使用不同的地址和密钥。

~~~yaml
models:
  - id: my-model
    name: 我的模型
    protocol: openai-chat
    base_url: https://provider.example/v1
    model: provider-model-id
    api_key_env: CURSOR_PROVIDER_API_KEY
~~~

- `id` 是配置里的唯一名称，`name` 是 Cursor 里的显示名，`model` 填供应商提供的真实模型 ID。
- `protocol` 按供应商的 API 选择 `anthropic`、`openai-chat` 或 `openai-responses`。`base_url` 填 API 基础地址，例如 `https://provider.example/v1`。服务会补上协议对应的请求路径。
- 每个模型选一种密钥配置。用 `api_key_env` 就在同一个终端设置对应变量。也可以把这一行替换成 `api_key: "你的密钥"`，直接在本地配置中填写。
- 供应商有额外要求时，可以设置 `headers`、`context_window`、`max_output_tokens`。Anthropic 模型还支持 `anthropic_thinking_effort`，可用 `low`、`medium`、`high`、`xhigh` 或 `max`。

~~~sh
export CURSOR_PROVIDER_API_KEY='你的密钥'
./target/release/cursor-server check --config config.yaml
~~~

PowerShell 设置变量用 `$env:CURSOR_PROVIDER_API_KEY = '你的密钥'`。如果在 YAML 里用了 `api_key`，可以跳过设置对应环境变量。`config.yaml` 和默认数据目录 `data/` 已加入 Git 忽略规则，请保存在本机。

## 3. 接入并启动 Cursor

1. 执行 `./target/release/cursor-server setup --config config.yaml`。首次运行会打印信任本地 CA 的系统命令。执行该命令，再运行一次 `setup`，直到提示 Cursor 设置已准备好。
2. 启动服务，并保持终端运行。

   ~~~sh
   ./target/release/cursor-server serve --config config.yaml
   ~~~

3. 在 Cursor 命令面板执行 **Developer: Reload Window**。新建 Agent 对话，在模型列表选择 `name` 对应的模型，发送「只回复 OK」做一次检查。

## 4. 日常操作

| 场景 | 操作 |
| --- | --- |
| 平时启动 | 设置所需的环境变量，运行 `serve`。改了源代码才需要重新构建。 |
| 更换模型、密钥或 API 地址 | 修改 YAML 并重启中转服务。在 Cursor 新开对话。若模型列表仍是旧的，再重载窗口。 |
| 端口被占用 | 修改 `server.listen` 和 `server.proxy_listen`，设为两个不同的本地端口。若改了代理端口，重新运行 `setup` 并重载 Cursor。 |
| 排查连通性 | 用 `doctor` 查看接入状态。用 `smoke` 向第一个模型发送一条短请求，可能产生 API 费用。命令均接 `--config config.yaml`。 |
| 停止并恢复 Cursor | 按 **Ctrl+C** 停止服务，执行 `./target/release/cursor-server restore --config config.yaml`，再重载 Cursor。需要撤销证书信任时，从系统信任库移除 `setup` 添加的本地 CA。 |

服务默认示例端口为 `43100`，代理端口为 `43101`。启动后可以访问 `http://127.0.0.1:43100/healthz`。Cursor 升级后如果出现 `Unknown model ID`，先重载窗口，再核对[兼容性记录](compatibility.md)。
