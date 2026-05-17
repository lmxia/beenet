# Beenet

基于 CID 的分布式 Wasm 任务网络，采用 Spin / Wasmtime 风格的嵌入式宿主架构。

- **CID 即函数地址**：Wasm 二进制哈希就是调用地址。
- **P2P 即调用总线**：Gateway / Agent 通过 libp2p 调 Worker。
- **Wasm 即计算体**：毫秒级实例化、强隔离。

> 完整架构、决策记录与里程碑见 [`target.md`](./target.md)。

## 现状（M1 / M1.5）

最短闭环已端到端跑通：

```text
curl → Gateway(HTTP) → libp2p → Worker → wasi:http/incoming-handler@0.2 → body 回传
```

**必须** 运行 **`beenet-registry`**：Worker 持 **join token** 对默认路径 **`POST /v1/workers/heartbeat`** 发送 **心跳**（首次即入网，后续为 **续租** / 保活；详见 [`target.md` §4.1 / §4.3](./target.md)）。Gateway 轮询 **`GET /v1/workers`** 刷新可拨号列表。

**Wasm 分发（推荐）**：`beenet-pack build` 后使用 **`beenet-pack upload`** 推到 **阿里云 OSS**（S3 兼容 API）；在 **`config.toml` 的 `[worker]`** 中配置 **`wasm_fetch_base`**，在 **`wasm_cache` 未命中** 时 **`GET {base}/{cid}`** 拉取并 **校验 CID** 后缓存（见 [`target.md` §3.1](./target.md)）。

档 0 接口走 W3C 标准 `wasi:http/incoming-handler@0.2`。任务作者可以直接用 `spin-sdk 5.x` 的 `#[http_component]` 写业务，Worker 侧通过 `wasmtime-wasi-http` 的 p2 `ProxyPre` 执行。

## 先决条件

- Rust stable（workspace 的 `rust-toolchain.toml` 已固定）
- `wasm32-wasip2` target：`rustup target add wasm32-wasip2`

## 配置文件

**Registry / Gateway / Worker / `beenet-pack upload`** 都从同一份 **TOML** 读运行参数：**不**再读取 `BEENET_*` 环境变量（日志仍可用 `RUST_LOG`，与 `tracing-subscriber` 一致）。

- **默认路径**：`dirs::config_dir()/beenet/config.toml`（常见 `~/.config/beenet/config.toml`；macOS 也可能是 `~/Library/Application Support/beenet/config.toml`）。
- **覆盖**：**`--config /path/to/config.toml`**（各二进制均支持）。

**优先级**：命令行里传入的 `--listen-addr`、`--registry-url` 等与 TOML 同名的 **flag** 会覆盖文件中对应字段；未在文件或 CLI 里给出的项使用代码内置默认值（与旧版 clap 默认一致）。

`beenet-pack build` / **`inspect`** **不需要** 该文件。缺少配置文件、或缺少当前进程所需的 table / 必填字段时，会直接报错退出。

开发与联调可共用一份配置，例如：

```toml
[registry]
http_addr = "127.0.0.1:3030"
join_token = "my-dev-token"

[gateway]
http_addr = "127.0.0.1:8080"
registry_url = "http://127.0.0.1:3030"
# registry_poll_ms = 2000
# default_deadline_ms = 10000

[worker]
listen_addr = "/ip4/127.0.0.1/tcp/4001"
registry_url = "http://127.0.0.1:3030"
join_token = "my-dev-token"
wasm_fetch_base = "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/beenet"
# wasm_cache_dir = "./wasm_cache"

[oss]
endpoint = "https://oss-cn-hangzhou.aliyuncs.com"
bucket = "my-bucket"
access_key_id = "LTAI..."
access_key_secret = "..."
region = "oss-cn-hangzhou"
# key_prefix = "beenet/"
# force_path_style = false
```

- **`[oss]`**：`beenet-pack upload` 必填 `endpoint`、`bucket`、`access_key_id`、`access_key_secret`、`region`；也可用 CLI **`--oss-endpoint`** 等逐项覆盖文件。
- **`wasm_fetch_base`**：不要末尾 `/`；实际请求 **`{base}/{cid}`**，需与 `upload` 写入的对象键一致。

## 端到端示例

以下命令假定当前目录为 **仓库根目录**（与 `Cargo.toml` 同级）。已在本机用 **`cargo build` 产物 + `examples/local-dev-config.toml` + 本地 `wasm_cache`** 跑通：**`GET /v1/workers` 可见 Worker，`curl` 返回 200**。

**配置文件**：可将上文「配置文件」中的 TOML 存到默认路径；或直接使用仓库里的 **`examples/local-dev-config.toml`**（仅本地缓存、无 `[oss]`），启动时加 **`--config examples/local-dev-config.toml`**。

### 1. 编译宿主与示例任务

```bash
# Host 二进制（registry / gateway / worker / pack）
cargo build --release --workspace

# 示例任务组件（wasm32-wasip2）
cargo build --release \
  --manifest-path examples/hello-filter-http/Cargo.toml \
  --target wasm32-wasip2
```

### 2. 打包成带 manifest 的单文件 wasm

```bash
mkdir -p dist wasm_cache

./target/release/beenet-pack build \
  --wasm examples/hello-filter-http/target/wasm32-wasip2/release/hello_filter_http.wasm \
  --manifest examples/hello-filter-http/beenet.toml \
  --out dist/task.wasm
```

输出形如：

```text
CID: …（以你本机 `beenet-pack inspect dist/task.wasm` 的输出为准，不同构建可能不同）
OUT: dist/task.wasm
SIZE: …
```

### 3. 发布到阿里云 OSS（推荐）

在 **`config.toml`** 的 **`[oss]`** 中填入 **RAM 子账号** 的 AK/SK（勿提交仓库）。典型 **华东1（杭州）**：[oss] 示例见上节。然后：

```bash
./target/release/beenet-pack upload --wasm dist/task.wasm
```

也可用 **`--oss-endpoint`、`--oss-bucket`、…** 覆盖文件中的单个字段。

若 PutObject 失败，可在 **`[oss]`** 中设 **`force_path_style = true`** 再试。

### 3b. 仅本地缓存（离线调试）

不写 OSS 时，可把打包产物拷入缓存：

```bash
CID=$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')
cp dist/task.wasm "wasm_cache/${CID}.wasm"
```

### 4. 启动 Registry（控制面）

```bash
RUST_LOG=info ./target/release/beenet-registry --config examples/local-dev-config.toml
```

监听地址与 **`join_token`** 来自配置文件中的 **`[registry]`**；也可用 **`--http-addr` / `--join-token`** 覆盖。

### 5. 启动 Worker

另开终端（同样先 `cd` 到仓库根目录，保证 `wasm_cache` 相对路径与配置一致）：

```bash
RUST_LOG=info ./target/release/beenet-worker --config examples/local-dev-config.toml
```

### 6. 启动 Gateway

```bash
RUST_LOG=info ./target/release/beenet-gateway --config examples/local-dev-config.toml
```

### 7. 发起请求

```bash
export CID="$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')"
curl -i -X POST "http://127.0.0.1:8080/run/ipfs/$CID" \
  --data 'please filter my badword please'
```

预期：

```text
HTTP/1.1 200 OK
x-beenet-status: ok
content-length: 27

please filter my *** please
```

若 **`connection refused`**：确认三进程已启动且端口未被占用。若 **`x-beenet-status: load-error`**：确认已执行 **§3b** 将 **`dist/task.wasm`** 拷到 **`wasm_cache/$CID.wasm`**，且 Worker 进程的工作目录与配置里的 **`wasm_cache_dir`** 一致（`examples/local-dev-config.toml` 使用相对路径 **`wasm_cache`**，须在仓库根目录启动 Worker）。

## 写你自己的任务

1. 新建一个 `wasm32-wasip2` 的 `cdylib` crate，依赖 `spin-sdk`，用 `#[http_component]` 导出 handler。
2. 写一份 `beenet.toml`（字段见 [`crates/beenet-manifest`](./crates/beenet-manifest) 和示例 [`examples/hello-filter-http/beenet.toml`](./examples/hello-filter-http/beenet.toml)），`interface` 固定为 `wasi:http/incoming-handler@0.2`。
3. 用 `beenet-pack build` 产出 `task.wasm`，**`upload` 到 OSS** 或拷入 `wasm_cache`，拿到 CID 后即可通过 Gateway 调用。

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `crates/beenet-common` | `BeenetCid`（CIDv1 / raw / sha2-256）+ 协议常量 |
| `crates/beenet-proto` | `InvokeRequest` / `InvokeResponse` / `Status` / `Usage` |
| `crates/beenet-manifest` | `beenet:manifest/v1` TOML schema + wasm custom section |
| `crates/beenet-pack` | `build` / `inspect` / **`upload`**（S3 兼容，OSS） |
| `crates/beenet-registry` | HTTP Registry：Worker **心跳** `POST /v1/workers/heartbeat`、`GET /v1/workers` |
| `crates/beenet-worker` | libp2p + Factors；Registry；**可选 HTTP 拉取 wasm** |
| `crates/beenet-gateway` | HTTP → libp2p；**必填** Registry URL，轮询 Worker 列表 |
| `examples/hello-filter-http` | `#[http_component]` 示例任务 |
| `examples/local-dev-config.toml` | 本地联调用 **`config.toml`**（registry / gateway / worker，无 OSS） |

## 许可

见 [`LICENSE`](./LICENSE)。
