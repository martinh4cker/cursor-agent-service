# Cursor Agent Service

Use your own model API in Cursor Agent through a local proxy configured with YAML. Build a single command line executable and run it alongside Cursor.

[简体中文使用教程](docs/guide.zh-CN.md) · [Cursor compatibility](docs/compatibility.md) · [MIT license](LICENSE)

## Quick start

1. Install the [stable Rust toolchain](https://rustup.rs/) and a native build toolchain. On macOS, use Xcode Command Line Tools. On Debian or Ubuntu, install `build-essential`, `pkg-config`, `libssl-dev`, and `cmake`. On Windows, use the Rust MSVC toolchain and Visual Studio Build Tools with C++.
2. Clone and build the service.

   ~~~sh
   git clone https://github.com/martinh4cker/cursor-agent-service.git
   cd cursor-agent-service
   cargo build --release --locked -p cursor-server
   cp config.example.yaml config.yaml
   ~~~

   The executable is `target/release/cursor-server`. On Windows, use `target/release/cursor-server.exe` and copy the example with `Copy-Item config.example.yaml config.yaml`.
3. Edit `config.yaml`. Replace `base_url` and `model` with the provider's base URL and model ID. Set the example key variable in the same terminal. In PowerShell, use `$env:CURSOR_PROVIDER_API_KEY = 'your-key'`.

   ~~~sh
   export CURSOR_PROVIDER_API_KEY='your-key'
   ./target/release/cursor-server check --config config.yaml
   ~~~
4. Run `setup`. Run the certificate trust command it prints, then run `setup` again until it reports that Cursor settings are prepared.

   ~~~sh
   ./target/release/cursor-server setup --config config.yaml
   ~~~
5. Start the service and reload Cursor with **Developer: Reload Window** from the command palette. Open an Agent chat, select the configured model by name, and send a short prompt. Keep the service running while Cursor uses its proxy.

   ~~~sh
   ./target/release/cursor-server serve --config config.yaml
   ~~~

## Model configuration

Add one entry under `models` for each model. Each entry can have a different provider and key.

~~~yaml
models:
  - id: my-model
    name: My model in Cursor
    protocol: openai-chat
    base_url: https://provider.example/v1
    model: provider-model-id
    api_key_env: CURSOR_PROVIDER_API_KEY
~~~

| Field | What to set |
| --- | --- |
| `id` and `name` | A unique configuration ID and the label to select in Cursor. |
| `protocol` | `anthropic` for Messages, `openai-chat` for Chat Completions, or `openai-responses` for Responses. |
| `base_url` | The provider's base URL, such as `https://provider.example/v1`. The service appends `messages`, `chat/completions`, or `responses` as appropriate. |
| `model` | The exact model identifier expected by the provider. |
| `api_key_env` or `api_key` | Exactly one per model. Supply an environment variable name, or put a quoted key in your local YAML. |

Optional per model fields include `headers`, `context_window`, and `max_output_tokens`. For Anthropic, `anthropic_thinking_effort` accepts `low`, `medium`, `high`, `xhigh`, or `max`. The service reads the configuration at startup. Restart it after editing models, then start a new Agent chat. Reload Cursor if the model picker still shows the old list.

The example uses ports `43100` for the service and `43101` for the proxy. Both can be changed under `server` to distinct loopback ports. After changing the proxy port, rerun `setup` and reload Cursor.

## Check or disconnect

- **Service status.** Run `cursor-server doctor --config config.yaml` with the executable from the build step, or open `http://127.0.0.1:43100/healthz` while the service is running.
- **Provider test.** Run `cursor-server smoke --config config.yaml` to send a short request to the first model. This can incur provider charges. A successful provider test confirms the API connection. A short prompt in Cursor verifies the Agent path.
- **Undo setup.** Stop the service with **Ctrl+C**, run `cursor-server restore --config config.yaml` with the same config and key variables, and reload Cursor. To remove the trusted certificate as well, remove the CA added by the `setup` command from the system trust store.

Setup installs a local proxy for Cursor and requires trusting its local CA so it can handle Cursor Agent traffic. The service binds to loopback. Keep `config.yaml`, the service data directory, and the local CA private. This project uses Cursor's internal protocol. See the [compatibility notes](docs/compatibility.md) for the tested setup and troubleshooting.

## Development

Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets`. On macOS or Linux, `./scripts/test.sh` also starts an isolated service and checks its health endpoint after building. See [UPSTREAM.md](UPSTREAM.md) for source attribution.
